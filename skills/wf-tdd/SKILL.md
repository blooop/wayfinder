---
name: wf-tdd
description: Build one ticket's worth of behavior test-first — red before green, one slice at a time, at seams the ticket pre-agreed — ending at a pushed branch with an open PR. Use when working a build-stage ticket, or when wf routes a build node here.
---

## Cross-agent invocation

This bundle runs in both Claude Code and Codex. Mention a wayfinder skill with
`/` in Claude Code and `$` in Codex; skill names below are written without a
sigil.

Build the behavior a **ticket** describes, test-first, and end at a **pushed branch with an open PR**. This skill is one stage of a node's lifecycle: it builds; it does not review (that is `wf-review`, in a fresh context), and it does not decide what to build (that already happened — the ticket is the contract).

Adapted from aihero.dev's `/tdd` + `/implement` (see `research/aihero-spine.md` in blooop/wayfinder). The loop discipline is theirs verbatim; every "ask the user" is rerouted to "read the ticket / post a comment and stop", because this skill must work with nobody in the room.

## Invocation

`wf-tdd <ticket> [ctx: <json>] [steer: <text>]` — an issue number or URL. Resolve the repo explicitly from the working checkout's remote and pass `--repo` on every `gh` call.

A launch from `wf` adds a `ctx: <json>` block after the ticket number: the parent map, the ticket's type and stage, and its linked PRs, all already fetched — so the map lookup this stage opens with is a read you can skip. It is an **accelerator, never a precondition**; a hand-typed invocation carries none and discovers exactly as it always has, and the claim below is a live call either way. See **The launch context** in [GITHUB_TRACKER.md](../wf/GITHUB_TRACKER.md).

## The contract is the ticket

1. **Claim first**: `gh issue edit <n> --repo "$REPO" --add-assignee @me` before any work. If already claimed, run the re-entry ritual: read the ticket body, the last `### handoff`, and any `**breadcrumb:**` comments after it.
2. **Read the ticket as the spec.** Open by restating what you will build — to yourself and as your first breadcrumb — rather than asking anyone what to build.
3. **Seams are pre-agreed, or you stop.** The ticket (or the spec it links) must name the seams to test at. Post the seams you intend to test as your first breadcrumb comment. **If the ticket names no seams, that is a gate failure**: comment asking for seams, leave a handoff, and stop. No test is written at an unconfirmed seam — the tracker stands in for the user.

## The loop

Work on a branch named `wayfinder/<repo-name>-<n>` off the default branch. Never commit to the default branch.

- **Red before green.** Write the failing test first, then only enough code to pass it. Don't anticipate future tests or add speculative features.
- **One slice at a time.** One seam, one test, one minimal implementation per cycle. Each cycle is a vertical slice — a narrow, complete path through the layers, demoable on its own.
- **Refactoring is not part of the loop.** It belongs to the review stage. Keep the diff minimal; resist cleanups the ticket didn't ask for.
- **Checkpoint habits**: typecheck regularly, run single test files regularly, and the full suite once at the end.

Anti-patterns — reject a test that is any of these:

- **Implementation-coupled**: breaks when you refactor but behavior hasn't changed. Renaming an internal function should break nothing in the suite.
- **Tautological**: the assertion recomputes the expected value the way the code does. Expected values are literals traceable to the ticket.
- **Horizontal slicing**: all tests first, then all implementation. Bulk tests verify imagined behavior.

Test names read as capabilities, not internals. Mock at **system boundaries only** — never your own classes or modules.

## Journaling

Post a `**breadcrumb:**` comment on the ticket at decision-grade moments (a seam settled, a direction change, a blocking question parked). Follow durability rules: behavioral not procedural, no file paths, no line numbers — a fresh agent may read this days later. On deliberate exit before the gate, post a `### handoff` (where we are / open thread / first move on resume).

## The gate — where this stage ends

Done means, in order:

1. Full test suite green locally.
2. Branch pushed **by name** to the pinned remote (never a bare `git push`).
3. PR opened against the default branch, its body written for a reviewer — see **The PR body** below.
4. A final breadcrumb on the ticket: PR number, one-line summary of the slice.

### The PR body

The ticket is your spec, but the PR's reader is a reviewer who has read neither the ticket nor the map. A body that opens with "slice 2 of #N, as decided in #M on map #K" makes them open four issues to learn why the PR exists, and that is the body reviewers complain about.

- **Use the host's PR skill when it has one** (`pr` in Claude Code, with `--no-review`: review is `wf-review`'s stage, not yours). It owns the body's shape; the rules below still hold.
- **The why first, in your own words**, one or two sentences, before any link. A link is where a reader goes for more, never where they go for the reason.
- **No map vocabulary** — slice, node, seam, stage, manager, prototype, or a term the ticket trail coined — unless the diff itself uses it. Say what the code does in the repo's own terms.
- **What changed**, one bullet per change the reviewer will see in the diff. Decisions settled on the ticket appear as their result, not as their history.
- **How you checked it**: the test that failed before and passes now, then the commands you ran, and what you did not run.
- **Merge danger**: whether a revert undoes it, and what breaks for whom if it is wrong.
- **`Closes #<n>` on its own line at the end** — it is what makes the PR↔ticket edge exist for stage derivation, so it is never optional, but it is not the opening line.

**On every return to this stage from `needs attention`, rewrite the body against the code as it now is**, before you push the fix. A body that describes the first round's code is wrong from the second round on.

Then **stop**. Do not review your own work, do not merge, do not continue into the next stage — the manager (see the `wf` skill's LIFECYCLE.md) or the human launches `wf-review` in a fresh context. If CI fails after push, fixing it is still this stage: fix, push, and only then stop.

If mid-stage you hit a question the ticket can't answer, degrade to: post the question as a comment, leave a handoff, exit. Never invent the answer and never wait in-session for one.
