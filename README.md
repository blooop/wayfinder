# wayfinder
wf: a multi-project wayfinder manager TUI — fuzzy-find picker and terminal starmap over agentic planning maps

## Installing

`wf` ships as a conda package on the `blooop` channel (published by the release
below):

```
pixi global install -c https://prefix.dev/blooop -c conda-forge wf
```

The package declares no run dependencies. `wf` finds `gh` (authenticated),
`zellij` and the agent CLI on PATH and says so plainly when one is missing,
rather than dragging second copies of them into its own environment.

`zellij` is optional: without it `enter` runs the agent in `wf`'s own terminal
instead of in a tab (see [Without zellij](#without-zellij)).

`wf --version` and `wf --help` are the only arguments; everything else is keys
in the TUI.

## Releasing

`recipe/recipe.yaml` builds the binary straight from the checkout, and
`.github/workflows/package.yml` is the only thing that builds it: on every pull
request and push to main it builds the recipe and re-installs the resulting
package into a throwaway environment to run `wf --version`. Pushing a
`v<version>` tag that matches `Cargo.toml` runs the identical build, then
attaches the package to a GitHub release and — if the repository has a
`PIXI_TOKEN` secret — publishes it to prefix.dev/blooop.

## Discovery

Projects accrete: running `wf` inside a git checkout with a GitHub `origin`
registers that checkout and opens focused on it (`ctrl-g` widens to every
known project, `ctrl-f` re-focuses the row's project). Run outside a checkout
to open on all of them.

The registry is a per-machine cache at `~/.cache/wf/projects.json`
(`$XDG_CACHE_HOME` respected) holding `{path, repo, session}` per checkout, plus
the map issue number the last search found for each repo. Deleting it is safe —
projects re-accrete as you open them, and the map numbers are re-found on the
next start.

Session names derive from the checkout path: the directory name, or the
*parent* directory name when several checkouts share a directory name
(`~/k1/kinisi_ros` → `k1`, `~/k2/kinisi_ros` → `k2`), falling back to the
home-relative path with `/` → `-` if names still collide.

Only repos with an open `wayfinder:map`-labelled issue show tickets; other
checkouts stay cached but hidden.

## Startup

`wf` draws its screen before it fetches anything, and fills it in as the data
arrives. Nothing is on the path to the first frame but the local `git` calls
that register the checkout, so the picker is up and answering keys immediately
— the count line says `searching for maps…`, then `loading maps 1/3`, then goes
quiet once everything is in.

Which repos have maps takes one `wayfinder:map` label search, and it used to be
the only genuinely serial step: nothing could load until it returned, which was
~2.5 s of a ~3 s start. So the map numbers are cached, and a warm start begins
fetching the maps themselves immediately — real tickets in ~0.6 s, and the cwd
focus applied on the *first* frame rather than a couple of seconds in. Every map
is fetched concurrently, so N projects cost one round trip rather than N.

The cache is a head start, never a skip: the search still runs on every start,
and its answer is what adds a repo mapped since last time, drops one whose map
was closed, and corrects a number that moved. A cached number is never taken at
its word either — a map fetch checks the issue is still an open `wayfinder:map`
and refuses it otherwise, so a stale number shows that project as stale for a
moment rather than rendering the wrong issue as its map. A search that fails is
retried rather than fatal, and the cwd focus yields to a scope you have already
chosen yourself with `ctrl-f`/`ctrl-g`.

## Launching

With zellij, picking a ticket creates or focuses a zellij tab in that project's session,
with the checkout as its cwd, running the `/wayfinder` skill on the ticket. The
tab is the unit of work and the unit of supervision: it survives `wf` exiting,
it is reachable by normal zellij navigation, and re-picking a ticket focuses its
existing tab instead of starting a second agent on it.

The tab is named `<repo>#<n> <short title>` — e.g.
`wayfinder#16 Build 4 — launch…` — with the title capped at 18 characters on a
word boundary, since zellij truncates the strip anyway. The `<repo>#<n>` half
is the tab's *identity*: it is what `wf` looks tabs up by, so retitling an issue
renames nothing and still finds the tab it already has. Everything that
recognises an agent tab (including the count in the AFK slot) reads that leading
key and ignores whatever follows it — the title, zellij's activity markers, or
both.

| key | what it does |
| --- | --- |
| `enter` | HITL: create-or-focus the ticket's tab and take you into it (`claude --dangerously-skip-permissions "/wayfinder <map> <n>"`) — with no zellij, run that agent right here |
| `ctrl-a` | AFK: spawn the same tab headless (`claude --dangerously-skip-permissions -p "/wayfinder <map> <n>"`) — no attach, no focus steal; needs zellij |
| `↑`/`↓`, `enter`, `esc` | in the which-checkout picker: pick which project hosts the tab, or cancel |

Both modes pass `--dangerously-skip-permissions`. For AFK that is closer to a
correctness requirement than a convenience: a headless agent that stops on a
permission prompt waits forever with nobody to answer it, and the only symptom
would be a tab that never finishes — indistinguishable from slow work. HITL gets
it too, so entering a tab is the same session whether you opened it or auto-start
did.

How `wf` hands over depends on where it is running, decided from whether there is
a `zellij` on PATH at all and then from its own `$ZELLIJ` — never from a
`zellij action` exit code, which is `0` even on failure:

- **no zellij installed** — the TUI suspends and the agent itself runs as a
  *child* in the checkout (see [Without zellij](#without-zellij));
- **outside zellij** — the TUI suspends, `zellij attach <session>` runs as a
  *child*, and detaching returns to `wf` (which refetches, since the tracker
  moved while you were away);
- **inside the project's own session** — the tab is focused and `wf` keeps
  running in its own tab;
- **inside another session** — zellij's session switcher gesture
  (`switch-session`) moves you over, and `wf` exits as it goes: the tab it was
  drawing in belongs to the session you just left, so nothing could reach it to
  quit it. Run `wf` again in the session you land in to pick the next ticket.

No new navigation keybindings: getting back is zellij's standard detach or
tab/session switching. The project's session is created detached if it does not
exist yet (rooted at the checkout; an EXITED session of that name is deleted
first so a stale layout cannot be resurrected).

A finished or crashed agent's tab lingers with an EXITED banner — that is the
post-mortem, and closing it is yours to do. The line above the match count
counts the agent tabs zellij is holding, as of the last launch, `ctrl-r`, or
auto-start; it stays empty when there are none.

## Without zellij

`zellij` is a nicety, not a requirement. When there is no `zellij` on PATH at all,
`enter` skips the tab entirely: `wf` steps out of the terminal and runs the agent
as its own child in the checkout, exactly the same
`claude --dangerously-skip-permissions "/wayfinder <map> <n>"`. Quitting the
agent returns to the picker, which refetches because the tracker moved while you
were in there. One ticket at a time, no tab strip, nothing to detach from.

That is a different state from "zellij is installed but this terminal is not in a
session" — the latter still gets a tab, and `wf` runs `zellij attach` to reach it.

Which of the two it is gets decided by one `zellij --version`, on the first
launch or poll that needs the answer and cached for the rest of the process —
never before the first frame, since every subprocess is kept off that path (#27)
and a zellij call is exactly the boundary that can wedge (#21).

What a tab was carrying is honestly absent rather than faked:

- `ctrl-a` is refused (`no zellij — afk needs a tab; enter runs this ticket here
  instead`). A headless agent is supervised by its tab; with no tab it would be a
  process you cannot see, find or reap, so `wf` will not start one;
- auto-start is off for the same reason — there is nothing to reconcile towards;
- the agent-tab count line stays empty, and re-picking a ticket starts a fresh
  agent on it, since there is no tab to find already running.

## Auto-start

While `wf` is running it keeps one invariant by itself: **every frontier
`research` ticket has a tab.** After each poll that came back healthy, it diffs
the frontier against the tabs that exist and spawns the missing ones as AFK
agents — the same `ctrl-a` seam, no keystroke. A research ticket unblocked on
another machine gets picked up on the next poll, so work happens while you are
actually away.

Only `wayfinder:research` is auto-started. `grilling` and `prototype` are HITL by
definition, and `task` is excluded on purpose: a build ticket running unattended
writes code and commits, which is a judgement for a keystroke now (`ctrl-a`) and
not for a label set weeks ago. A ticket with no recognised type label is never
auto-started either.

It only ever creates:

- an existing tab — running **or** EXITED — means the ticket is not started
  again, so a dead agent's corpse is the "don't retry" record and you retry by
  closing the tab;
- a repo whose latest poll failed is skipped until one succeeds, since a stale
  frontier could open a tab for a ticket already closed elsewhere;
- a ticket that has left the frontier (claimed, blocked, closed) is left alone,
  and no tab is ever closed for you.

A tab that appears on its own is announced: the count line reads
`auto-started wayfinder#3` and the agent-tab count is recounted on the spot, so
the screen never disagrees with the tab bar about a tab nobody asked for.

There is no off switch, no launch stagger and no fan-out cap: quitting `wf` is
the switch.
