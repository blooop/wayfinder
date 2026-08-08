---
name: tdd
description: Build one ticket's worth of behavior test-first — red before green, one slice at a time, at seams the ticket pre-agreed — ending at a pushed branch with an open PR. Use when working a build-stage ticket, or when wf routes a build node here.
---

Build the behavior a **ticket** describes, test-first, and end at a **pushed branch with an open PR**. This skill is one stage of a node's lifecycle: it builds; it does not review (that is `/review`, in a fresh context), and it does not decide what to build (that already happened — the ticket is the contract).

Adapted from aihero.dev's `/tdd` + `/implement` (see `research/aihero-spine.md` in blooop/wayfinder). The loop discipline is theirs verbatim; every "ask the user" is rerouted to "read the ticket / post a comment and stop", because this skill must work with nobody in the room.

## Invocation

`/tdd <ticket>` — an issue number or URL. Resolve the repo explicitly from the working checkout's remote and pass `--repo` on every `gh` call.

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
3. PR opened against the default branch, its body linking the ticket (`Closes #<n>` so the PR↔ticket edge exists for stage derivation).
4. A final breadcrumb on the ticket: PR number, one-line summary of the slice.

Then **stop**. Do not review your own work, do not merge, do not continue into the next stage — the manager (see the wayfinder skill's LIFECYCLE.md) or the human launches `/review` in a fresh context. If CI fails after push, fixing it is still this stage: fix, push, and only then stop.

If mid-stage you hit a question the ticket can't answer, degrade to: post the question as a comment, leave a handoff, exit. Never invent the answer and never wait in-session for one.
