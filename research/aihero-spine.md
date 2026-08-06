# aihero.dev skills — the workflow spine, read from primary sources

Research notes for authoring wayfinder's `/tdd` and `/review` skills. 2026-08-06.

## What was accessible

Everything. Nothing load-bearing was paywalled.

- **Marketing/docs site**: https://www.aihero.dev/skills lists the collection; per-skill pages
  (e.g. https://www.aihero.dev/skills-tdd, https://www.aihero.dev/skills-to-prd) were fetched and
  showed no paywall.
- **Primary source**: the actual SKILL.md files live in **https://github.com/mattpocock/skills**
  (Matt Pocock's repo, shipped as a Claude Code plugin). Every SKILL.md quoted below was read
  verbatim from `raw.githubusercontent.com/mattpocock/skills/main/...`. The repo also carries a
  `docs/` tree (the site content) whose "It's working if" sections are each skill's acceptance
  test, plus a `CONTEXT.md` domain glossary and ADRs.
- **Naming lineage**: `/to-prd` was renamed to `/to-spec` in v1.1 — "'Spec' is now the single
  through-line term, and the old `to-prd` slug is dead"
  (https://www.aihero.dev/skills-to-prd). `/to-issues` is now `/to-tickets`
  (https://www.aihero.dev/skills-to-issues redirects to the to-tickets content). There is no
  separate "PR review" skill; review-shaped work is `/code-review`.
- Not found anywhere: a skill that relies on a long-running orchestrator process. Chaining is done
  by one skill's text naming the next skill, inside one interactive session.

## The spine

The repo's own docs state the chain explicitly, in multiple places:

> "grill-with-docs → to-spec → to-tickets → implement → code-review"
> — https://www.aihero.dev/skills-to-prd, https://www.aihero.dev/skills-to-issues, and the
> `docs/engineering/*.md` "Where it fits" sections.

Handoffs are **by convention, not by state machine**: `to-spec` and `to-tickets` publish to "the
issue tracker" (an abstraction bound per-repo by `/setup-matt-pocock-skills`); `implement` reads a
ticket and invokes `/tdd`, then literally says "Once done, use /code-review"; `code-review` finds
its spec by following issue references in commit messages back to the tracker. The tracker is the
only durable shared state in the spine — the rest is in-session context.

One extra skill matters here: Matt's own **`/wayfinder`** skill is the direct ancestor of
blooop/wayfinder's data model (`wayfinder:map` label, child tickets, claim-by-assignee, native
blocking edges, frontier query). Read it for the concurrency rules.

---

## /tdd

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/tdd/SKILL.md
(+ `tests.md`, `mocking.md` in the same folder;
docs: https://github.com/mattpocock/skills/blob/main/docs/engineering/tdd.md)

**Contract.** Model-invoked *reference* skill, not an orchestrator — "the reference that makes
that loop produce tests worth keeping". Input: a concrete behavior to build, plus **pre-agreed
seams**. Output: tests + minimal implementations, produced one vertical slice at a time. It is
deliberately stateless: it writes no tracker state and produces no handoff artifact of its own.

Load-bearing quotes:

> "**Test only at pre-agreed seams.** Before writing any test, write down the seams under test and
> confirm them with the user. No test is written at an unconfirmed seam."

> "**Red before green.** Write the failing test first, then only enough code to pass it. Don't
> anticipate future tests or add speculative features."

> "**One slice at a time.** One seam, one test, one minimal implementation per cycle."

> "**Refactoring is not part of the loop.** It belongs to the review stage (see the `code-review`
> skill), not the red → green implementation cycle."

Anti-patterns it names (SKILL.md): **implementation-coupled** ("the test breaks when you refactor
but behavior hasn't changed"), **tautological** ("the assertion recomputes the expected value the
way the code does … Expected values must come from an independent source of truth"), and
**horizontal slicing** ("writing all tests first, then all implementation. Bulk tests verify
_imagined_ behavior"). Mocking rule (`mocking.md`): "Mock at **system boundaries** only … Don't
mock: your own classes/modules."

**Gates / "done"** (docs/engineering/tdd.md, "It's working if"):

- "It stops and names the seams it intends to test at, and waits, before any test file exists."
- "One test appears, goes red, gets just enough code to pass, and only then does the next test appear."
- "Test names read as capabilities … not as internals."
- "Expected values in assertions are literals you can trace to the spec."
- "Renaming an internal function breaks nothing in the suite."
- "Mocks appear only at external boundaries."

**Chaining.** Explicitly defers refactoring to `/code-review` by name. Upstream, seams are meant to
be settled by `/to-spec` ("Sketch out the seams at which you're going to test the feature … Check
with the user that these seams match their expectations" — to-spec SKILL.md), so by the time /tdd
runs the seams should already be in the spec/ticket.

**State assumptions.** Reads `CONTEXT.md` "(if it exists)" and ADRs for vocabulary; otherwise
nothing but the working tree. Assumes a *user is present* to confirm seams if they weren't
pre-agreed.

## /code-review

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/code-review/SKILL.md
(docs: https://github.com/mattpocock/skills/blob/main/docs/engineering/code-review.md)

**Contract.** Input: a **fixed point** ("a commit SHA, branch name, tag, `main`, `HEAD~5` … If they
didn't specify one, ask for it") and a spec source. Output: a two-axis report — `## Standards` and
`## Spec` — delivered in-session. It reviews; it does not fix.

Process, quoted:

> "Capture the diff command once: `git diff <fixed-point>...HEAD` (three-dot, so the comparison is
> against the merge-base)."

> "Before going further, confirm the fixed point resolves (`git rev-parse <fixed-point>`) and the
> diff is non-empty. A bad ref or empty diff should fail here — not inside two parallel sub-agents."

Spec discovery order: "1. Issue references in the commit messages (`#123`, `Closes #45` …) — fetch
via the workflow in `docs/agents/issue-tracker.md`. 2. A path the user passed … 3. A spec file
under `docs/`, `specs/`, or `.scratch/` … 4. … ask the user … If they say there isn't one, the
**Spec** sub-agent will skip and report 'no spec available'."

Standards axis = repo-documented standards **plus a fixed 12-smell Fowler baseline** (Mysterious
Name, Duplicated Code, Feature Envy, Data Clumps, Primitive Obsession, Repeated Switches, Shotgun
Surgery, Divergent Change, Speculative Generality, Message Chains, Middle Man, Refused Bequest),
with two binding rules: "**The repo overrides**" and "**Always a judgement call** … skip anything
tooling already enforces."

Both axes run as "**parallel sub-agents** so they don't pollute each other's context", each capped
at "Under 400 words". Aggregation rule: "Do **not** merge or rerank findings … Don't pick a single
winner across axes — that's the reranking the separation exists to prevent." Rationale: "Code that
follows every standard but implements the wrong thing → Standards pass, Spec fail" and vice versa.

**Gates / "done"** (docs): "It refuses to start on a bad ref or an empty diff, before any sub-agent
is spawned"; "every Spec finding quotes a line of the spec"; "With no spec available, the Spec
block says so instead of listing requirements it inferred from the code."

**Chaining.** Terminal stage of the spine; invoked by name from `/implement`. It owns the
refactoring conversation that `/tdd` refuses to have.

**State assumptions.** Git history + the tracker (via `docs/agents/issue-tracker.md`); a user
present to supply the fixed point if unstated. The report itself is **not persisted anywhere** —
it lives and dies in the session.

## /implement

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/implement/SKILL.md
(docs: https://github.com/mattpocock/skills/blob/main/docs/engineering/implement.md)

The whole skill is six lines — quoted in full:

> "Implement the work described by the user in the spec or tickets.
> Use /tdd where possible, at pre-agreed seams.
> Run typechecking regularly, single test files regularly, and the full test suite once at the end.
> Once done, use /code-review to review the work.
> Commit your work to the current branch."

**Gates** (docs "It's working if"): "The session opens by reading the ticket or spec and restating
what it will build, rather than asking you what to build"; "The run reaches a commit on your
current branch without you prompting it to carry on"; "The diff is one ticket's worth of change: a
vertical slice through every layer, not several tickets swept together."

**Chaining/state.** This *is* the aihero orchestrator: it chains `/tdd` → `/code-review` **inside
one session** and its durable output is a commit on the current branch. No PR, no issue comment,
no state file.

## /to-spec (formerly /to-prd)

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/to-spec/SKILL.md
(rename confirmed at https://www.aihero.dev/skills-to-prd)

**Contract.** Input: the current conversation ("Do NOT interview the user — just synthesize what
you already know" — grilling already happened upstream). Output: a spec published **to the issue
tracker**, using a fixed template (Problem Statement / Solution / User Stories / Implementation
Decisions / Testing Decisions / Out of Scope / Further Notes), labelled `ready-for-agent`.

Load-bearing quotes:

> "Sketch out the seams at which you're going to test the feature. Existing seams should be
> preferred to new ones. Use the highest seam possible. … The fewer seams across the codebase, the
> better - the ideal number is one. Check with the user that these seams match their expectations."

> "Do NOT include specific file paths or code snippets. They may end up being outdated very
> quickly."

**Chaining.** Downstream, `/to-tickets` slices the spec; the seams section is what `/tdd` later
treats as "pre-agreed". **State**: the spec is a tracker issue (GitHub issue or `.scratch/` file),
not an in-repo doc.

## /to-tickets (formerly /to-issues)

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/to-tickets/SKILL.md
(docs: https://github.com/mattpocock/skills/blob/main/docs/engineering/to-tickets.md)

**Contract.** Input: "a plan, spec, or the current conversation" (or an issue number/URL argument —
"fetch it and read its full body and comments"). Output: one issue per ticket on the tracker, in
dependency order, with **blocking edges** ("Use the platform's native blocking / sub-issue
relationship where it has one"), labelled `ready-for-agent` — "the tickets are agent-grabbable by
construction". "Do NOT close or modify any parent issue."

Slicing rules, quoted:

> "Each slice cuts a narrow but COMPLETE path through every layer (schema, API, UI, tests) —
> vertical, NOT a horizontal slice of one layer … A completed slice is demoable or verifiable on
> its own … **Each slice is sized to fit in a single fresh context window**."

Issue template: `## Parent` / `## What to build` ("the end-to-end behaviour this ticket makes work,
from the user's perspective — not layer-by-layer implementation") / `## Acceptance criteria`
(checkboxes) / `## Blocked by`. Same durability rule as to-spec: "avoid specific file paths or code
snippets — they go stale fast."

Wide refactors get an explicit exception: sequence as **expand–contract** ("First expand: add the
new form beside the old so nothing breaks. Then migrate the call sites over in batches … Finally
contract"), each batch its own ticket.

**Gates** (docs "It's working if"): "Every ticket has an answer to 'what can I demo when this is
done?'"; "The ticket at the top has no blockers and can be started immediately"; "**Each ticket
reads like something a fresh session could finish without you in the room.**"

**Chaining.** HITL checkpoint before publish ("Present the proposed breakdown as a numbered list …
Iterate until the user approves"). Execution rule for consumers: "Work the **frontier**: any ticket
whose blockers are all done."

## /triage (+ AGENT-BRIEF.md)

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/triage/SKILL.md and
https://github.com/mattpocock/skills/blob/main/skills/engineering/triage/AGENT-BRIEF.md

**Contract.** Moves issues (and optionally external PRs — "a PR is an issue with attached code")
through a label state machine: categories `bug`/`enhancement`; states `needs-triage`, `needs-info`,
`ready-for-agent`, `ready-for-human`, `wontfix`. "Every triaged issue should carry exactly one
category role and one state role." Every generated comment must start with
"`> *This was generated by AI during triage.*`". Verification is a gate: "Before any grilling,
check that the claim holds up. For a bug, reproduce it from the reporter's steps."

The `ready-for-agent` outcome posts an **agent brief** comment. AGENT-BRIEF.md is the best document
in the repo for wayfinder's purposes:

> "The original body and discussion are context — **the agent brief is the contract**."

> "**Durability over precision.** The issue may sit in `ready-for-agent` for days or weeks. …
> **Don't** reference file paths — they go stale. **Don't** reference line numbers."

> "**Behavioral, not procedural.** Describe **what** the system should do, not **how** to implement
> it. The agent will explore the codebase fresh and make its own implementation decisions."

> "**Complete acceptance criteria.** The agent needs to know when it's done. … Each criterion
> should be independently verifiable."

> "**Explicit scope boundaries.** State what is out of scope. This prevents the agent from
> gold-plating."

Brief template: Category / Summary / Current behavior / Desired behavior / Key interfaces /
Acceptance criteria (checkboxes) / Out of scope.

**State assumptions.** The tracker, plus a local `.out-of-scope/*.md` knowledge base for rejected
enhancements (a committed repo directory).

## /grill-with-docs, /grill-me, /grilling

Sources: https://github.com/mattpocock/skills/blob/main/skills/engineering/grill-with-docs/SKILL.md,
https://github.com/mattpocock/skills/blob/main/skills/productivity/grill-me/SKILL.md,
https://github.com/mattpocock/skills/blob/main/skills/productivity/grilling/SKILL.md

`grill-me` and `grill-with-docs` are one-liners over the `/grilling` primitive ("Run a `/grilling`
session, using the `/domain-modeling` skill"). Grilling's contract:

> "Interview the user relentlessly until you reach a shared understanding. Map this as a **design
> tree** … The **frontier** is every decision whose prerequisites are already settled … Ask the
> whole frontier in one round: number each question and give your recommended answer."

> "Finding _facts_ is your job, never the user's. When a frontier question needs a fact from the
> environment … dispatch a sub-agent to find it."

> "The session is done when the frontier is empty … Do not act on it until the user confirms you
> have reached a shared understanding."

Strictly HITL by design; the wayfinder skill even codifies "a grilling agent that answers its own
questions has broken this". Output is conversation state (plus CONTEXT.md/ADR updates in the
-with-docs variant) — consumed immediately by `/to-spec`.

## /wayfinder (Matt's — ancestor of this project's model)

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/wayfinder/SKILL.md and
https://github.com/mattpocock/skills/blob/main/skills/engineering/setup-matt-pocock-skills/issue-tracker-github.md

Planning-only ("Wayfinder is **planning** by default … produce decisions, not deliverables") map of
*decision tickets* — but its tracker mechanics are exactly blooop/wayfinder's substrate:

- "The map is a single issue … labelled `wayfinder:map`"; tickets are child issues; ticket body
  "sized to one 100K token agent session".
- **Claim protocol**: "A session **claims** a ticket by assigning it to the dev driving the map,
  **first**, before any work, so concurrent sessions skip it. That assignee _is_ the claim."
- **Blocking**: "the tracker's **native** dependency relationship — essential because it renders the
  frontier _visually_"; "the **frontier** is the open, unblocked, unclaimed children".
- **Resolution protocol**: "post the answer as a **resolution comment**, **close** the issue, and
  **append a context pointer** to the map's Decisions-so-far."
- **Budget**: "**never resolve more than one ticket per session** — with the exception of research
  tickets."
- **Concurrency stance**: "expect other sessions to be editing the tracker concurrently."

The GitHub binding doc gives the exact `gh` idioms (create/comment/label/close, dependencies via
`gh api …/dependencies/blocked_by` with the blocker's **database id**, claim via
`gh issue edit <n> --add-assignee @me` as "the session's first write").

## /handoff

Source: https://github.com/mattpocock/skills/blob/main/skills/productivity/handoff/SKILL.md

"Write a handoff document summarising the current conversation so a fresh agent can continue the
work. **Save to the temporary directory of the user's OS - not the current workspace.**" Includes a
"suggested skills" section; "Do not duplicate content already captured in other artifacts (specs,
plans, ADRs, issues, commits, diffs). Reference them by path or URL instead"; redact secrets.
Note the state choice: the handoff is a *local temp file* — deliberately ephemeral.

## /research

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/research/SKILL.md

Background agent; "Investigate the question against **primary sources** … Write the findings to a
single Markdown file, citing each claim's source." In the wayfinder skill, research tickets capture
"findings on a throwaway `research/<name>` branch with a context pointer from the ticket" — i.e.
branch + issue comment as the handoff, the one aihero pattern that already matches blooop/wayfinder
exactly.

## The configuration indirection

Source: https://github.com/mattpocock/skills/blob/main/skills/engineering/setup-matt-pocock-skills/SKILL.md

Every tracker-touching skill says "The issue tracker should have been provided to you — run
`/setup-matt-pocock-skills` if not", and reads `docs/agents/issue-tracker.md` (a committed file
binding abstract operations to `gh` commands, `glab`, or `.scratch/` files). Skills never hardcode
`gh`; the binding doc does. Labels get the same treatment via `docs/agents/triage-labels.md`.

---

# Adopt / Adapt / Reject for wayfinder's /tdd and /review

Consumer model being evaluated against: GitHub Issues are the **only** shared state (breadcrumbs =
issue comments); branch + PR is the durable artifact; `wf` is an exec-and-exit launcher; a manager
runs each stage as a **fresh subagent** with gates between them (PR opened, checks green) — no
in-session memory survives a stage boundary.

## Adopt (as written)

1. **The entire /tdd loop discipline** — red-before-green, one-slice-at-a-time, tracer bullets, the
   three named anti-patterns (implementation-coupled / tautological / horizontal slicing), the
   mock-at-boundaries-only rule, and "test names read as capabilities". This is all stateless
   reference material; it works identically in a fresh subagent. Source: tdd/SKILL.md.
2. **"Refactoring is not part of the loop … it belongs to the review stage."** This is exactly the
   stage separation wayfinder's manager enforces; adopting it keeps the /tdd stage's diff minimal
   and gives /review a defined job. Source: tdd/SKILL.md.
3. **Two-axis review (Standards vs Spec), parallel subagents, no merged ranking.** The rationale
   ("one axis masking the other") holds regardless of orchestration, and per-axis word caps keep
   the report postable as a PR comment. Source: code-review/SKILL.md.
4. **Fail-fast preflight before spawning review subagents** — "confirm the fixed point resolves …
   and the diff is non-empty. A bad ref or empty diff should fail here — not inside two parallel
   sub-agents." Perfect gate semantics for a manager: cheap, checkable, refuses to burn a stage.
5. **The Fowler 12-smell baseline with its two binding rules** ("the repo overrides"; "always a
   judgement call"; "skip anything tooling already enforces"). Gives /review teeth on a repo with
   thin documented standards, without turning it into a linter cosplay.
6. **AGENT-BRIEF durability rules** for everything posted to tickets: behavioral not procedural, no
   file paths, no line numbers, checkbox acceptance criteria, explicit out-of-scope. Wayfinder's
   breadcrumb comments should follow these verbatim — they're written for exactly the
   "fresh agent reads it days later" case. Source: triage/AGENT-BRIEF.md.
7. **"Each slice is sized to fit in a single fresh context window" / "reads like something a fresh
   session could finish without you in the room."** This is wayfinder's fresh-subagent model stated
   as a ticket-authoring rule. Source: to-tickets SKILL.md + docs.
8. **Claim-first, resolution-comment-then-close, one-ticket-per-session, expect concurrent editors**
   from Matt's /wayfinder — blooop/wayfinder already inherits this substrate; keep the protocol
   wording. Source: wayfinder/SKILL.md, issue-tracker-github.md.
9. **The AI-disclaimer line on generated tracker comments** ("> *This was generated by AI during
   triage.*") — cheap provenance for breadcrumb comments. Source: triage/SKILL.md.

## Adapt (right idea, wrong substrate)

1. **Seam pre-agreement.** aihero gates /tdd on a *live user confirming seams* ("confirm them with
   the user … No test is written at an unconfirmed seam"). A fresh AFK subagent has no user.
   Adaptation: the seams are agreed at planning time and written into the **ticket** (spec/brief
   section); wayfinder's /tdd reads seams from the issue, posts the seams it's testing at as its
   first breadcrumb comment, and if the ticket names none, that's a gate failure — comment asking
   for seams and stop, rather than inventing them. Keeps the "no test at an unconfirmed seam"
   invariant with the tracker standing in for the user.
2. **Review inputs.** "If they didn't specify one, ask for it" (fixed point) and step-4 "ask the
   user where the spec is" become deterministic in wayfinder: fixed point = the PR's merge-base
   with the base branch; spec = the ticket the branch/PR references. Adopt the discovery *order*
   (issue refs in commits first) as the fallback; drop the interactive fallbacks. The "no spec →
   Spec axis reports 'no spec available'" behavior is worth keeping verbatim — never infer the spec
   from the code.
3. **Review output.** aihero's report is in-session only — with exec-and-exit that evaporates.
   Post the two-axis report as a **PR review/comment** (and a gist breadcrumb on the ticket), so
   the gate ("review posted, findings addressed") is checkable by the manager from GitHub state
   alone.
4. **`/implement`'s checkpoint habits** — "typechecking regularly, single test files regularly, and
   the full test suite once at the end", "commit your work to the current branch" — fold into
   wayfinder's /tdd stage, but the *sequencing* role of /implement (invoking /code-review) moves to
   the manager. End state adapts from "a commit on the current branch" to "a pushed branch with an
   open PR".
5. **/handoff's content rules, not its medium.** "Do not duplicate content already captured in
   other artifacts … reference them by path or URL" and secret redaction are exactly right for
   breadcrumb comments; the OS-temp-dir destination is the opposite of wayfinder's only-shared-
   state-is-GitHub rule. Handoffs are ticket comments, full stop.
6. **Tracker indirection.** The `docs/agents/issue-tracker.md` binding layer is over-general for
   wayfinder (GitHub is the substrate, `wf` depends on `gh`). Skip the indirection; steal the
   concrete `gh` idioms from issue-tracker-github.md (including dependencies-API details and
   claim-by-assignee) directly into the skills.

## Reject

1. **In-session skill chaining as the spine.** `/implement` calling `/code-review` "once done" in
   the same context is the load-bearing orchestration move, and it assumes a session that survives
   both stages. Wayfinder's manager + gates replaces it; a wayfinder /tdd skill must *end* at its
   gate (PR opened, checks green) and never "continue into" review itself. Reviewing your own
   fresh work in the same context also forfeits the fresh-eyes benefit wayfinder's model buys.
2. **Mid-stage HITL rounds.** Grilling ("wait for the user's answers"), to-tickets' "iterate until
   the user approves", triage's "wait for direction" — all fine at planning time, fatal inside an
   AFK stage. Any wayfinder stage question must degrade to: post a comment, fail the gate, exit.
3. **Local-file shared state.** `CONTEXT.md`/ADRs are fine (they're committed and ride the branch),
   but `.scratch/` local tickets, OS-temp handoff docs, and any state that isn't a GitHub issue,
   comment, branch, or PR violates the one-source-of-truth rule and silently disappears between
   `wf` invocations on different machines.
4. **The in-session review report as the deliverable.** (Covered in Adapt-3, but the default must
   be rejected explicitly: a review that only ever existed in a terminated session did not happen,
   as far as the manager's gate is concerned.)
5. **"Commit to the current branch" as the finish line.** Without push + PR there is no durable
   artifact and no gate; aihero stops one step short of wayfinder's minimum.

## One-paragraph synthesis

aihero's spine and wayfinder's spine are the same shape — grill → spec → tickets → tdd → review —
and its ticket-facing rules (frontier, claim-by-assignee, agent briefs, fresh-context sizing,
durability-over-precision) transplant directly because they were already written for AFK agents
reading GitHub state cold. What does *not* transplant is the connective tissue: aihero chains
stages through a living session and a present user, where wayfinder chains them through a manager,
gates, and issue comments. Author /tdd and /review by keeping aihero's loop discipline, gate
predicates, and comment templates verbatim, while rerouting every "ask the user" to "read the
ticket / post a comment and stop", and every in-session output to a PR or issue artifact.
