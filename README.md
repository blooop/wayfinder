# wayfinder
wf: a multi-project wayfinder ticket selector — a fuzzy-find picker over every
project's wayfinder tickets. Pick one and the agent runs right there.

## Installing

`wf` ships as a conda package on the `blooop` channel (published by the release
below):

```
pixi global install -c https://prefix.dev/blooop -c conda-forge wf
```

The package declares no run dependencies. `wf` finds what it needs on PATH and
says so plainly when something is missing, rather than dragging second copies
into its own environment. It wants an authenticated `gh` and the agent CLI, and
nothing else — there is no multiplexer to install.

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
(`$XDG_CACHE_HOME` respected) holding `{path, repo}` per checkout, plus every
open map that the last search found. Deleting it is safe — projects re-accrete
as you open them, and the maps are re-found on the next start.

Only repos with an open `wayfinder:map`-labelled issue show tickets; other
checkouts stay cached but hidden.

**Every open map renders as its own cluster** — a `▌ repo · map title` header
carrying the per-**status** counts (`○` frontier `◐` claimed `⊘` blocked `●`
done). Those are the ticket's own state, not the stage its rows draw below.
A repo can keep several maps open at once and each gets its own cluster;
focusing a project shows all of them.

**The default screen is the leverage view**: each cluster shows its takeable
tickets (frontier and claimed), sorted by how many open tickets each one
unblocks, with that unblocks-subtree drawn beneath it — so the next ticket is
chosen by what taking it unlocks, not by status alone. Rows are
`<glyph> #n <title> [type] ⇄ PR#n <state>` — the `⇄` badges are the ticket's
linked pull requests (GitHub's Development-panel set: closing keywords and
manual links), shown as `draft`/`open`/`merged`/`closed` with `✓`/`✗` on an
open PR when its checks and review are settled or need action.

The leading glyph is the row's **stage**: `○` ready · `◐` building (in progress,
for a decision node) · `◍` in review · `!` needs attention · `●` done, with `⊘`
overriding on a blocked node, whose stage is unactionable until its blockers
clear. Stage is derived from the linked PRs first and the ticket's own state
only when they say nothing: an open PR with failing checks or a
changes-requested review is needs-attention, a draft or pending checks is
building, any other open PR is in review, and a node takes the **worst** of its
open PRs — one red PR makes the node red. With no open PR, a merged one means
done; with no PR at all it falls back to unclaimed → ready, claimed → building,
closed → done.

`↑`/`↓` prefer **siblings at the cursor's depth**, so on the default screen they
move between tickets you can actually take and step over the blocked context
hanging beneath them; `→` reveals what the cursor is on and `←` closes it again.
Depth 0 spans clusters, so `↓` still carries you from one project into the next.

Every direction key **always navigates**: held down, each one keeps moving until
it reaches its own end of the list, and none of them can strand the cursor. A
preference is only a preference — where there is no sibling to walk to (a ticket
that is an only child, say) `↑`/`↓` step to the neighbouring row instead, and
`←` falls back the same way once there is no parent left to climb to. `→` steps
one stop at a time, so it is the key that visits everything.

The cursor is a bold **orange `▶`** sitting directly against the item it points
at rather than in a left-hand gutter, so it steps visibly rightward as you
descend and the depth axis is something you can see:

```
    ○ #1069 Prototype the reconciled on-disk form
    └─  ⊘ #1070 A3 disposition…
      ├─  ⊘ #1071 A4 disposition…
      │ ├─▶ ⊘ #1073 Dispositions for plans…
```

Orange because it is the one hue the screen does not otherwise spend — cyan is
cluster headers and the prompt, green/yellow/red the status glyphs and counts,
magenta the PR badges, dim everything settled — so the selection never competes
with something that means something else. The branch furniture stays uniformly
dim: it is structure, not status.

Done work collapses to a per-cluster `▸ ● N done (hidden)` line and blocked
tickets no subtree reaches to `▸ ⊘ N blocked deeper down`. **While shut, each
carries a stage rollup** of what it is holding — the same glyphs as counts,
`◍1 ●3` — so a branch you folded away still says whether anything in it wants
you. Each held node is counted once, and only a shut row has a rollup: open it
and the rows themselves are right there. Both are ordinary cursor stops: put
the cursor on one and `→` opens it in place (`▾`), listing what it held as rows
you can select and launch, and `←` shuts it again — so nothing is ever merely a
number you cannot reach. A map with nothing takeable
leaves the body entirely, counted on the bottom line as `· N idle maps hidden`.

**`tab` shows the structure forest** instead: the whole blocking DAG, done
tickets dimmed in place. A ticket's tree parent is its lowest-numbered in-map
blocker; edges the tree cannot show are annotated `⤷ also needs #n`.

**Typing flattens**: a live query replaces the tree with one flat list ordered
by fuzzy-match score across every project (rows name their repo); clearing it
restores whichever screen `tab` had toggled.

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
and refuses it otherwise, so a stale number renders *nothing* for a moment
rather than the wrong issue as that project's map. A search that fails is
retried rather than fatal, and the cwd focus yields to a scope you have already
chosen yourself with `ctrl-f`/`ctrl-g`.

Each map is fetched exactly once. Nothing polls: `wf` is on screen for seconds
and restarts warm in ~0.6 s, so there is nothing worth keeping fresh. `ctrl-r`
refetches everything when you want it — after closing a ticket in the browser,
say. A fetch that fails says so on the count line and stays failed until you
ask again.

## Launching

Picking a ticket runs an agent on it — `claude --dangerously-skip-permissions
"<skill> …"`, with that project's checkout as its cwd — and that is the end of
`wf`. It restores the terminal and **replaces its own process** with the agent:
no picker underneath, no parent waiting, nothing to detach from. The agent holds
the terminal, the exit code and the signals directly, because by then it *is*
the process you started.

**Launching is two steps.** `enter` on the cursor's ticket does not launch: it
opens the **launch line** where the count line was, showing where `enter` will
go — `→ /tdd · #65 Author the /tdd skill`. What you type lands on that line, not
in the query, and *is* the mode:

| the line says | what launches |
| --- | --- |
| *(empty)* | interactive — the default |
| `defer` | the subtree, resolved unattended |
| `defer <text>` | deferred, with `<text>` as the steering prompt |
| *anything else* | interactive, with what you typed as the steering prompt |

`esc` backs out to the list with your query and cursor exactly as they were.
`enter` on a **done** or **blocked** node opens nothing — it says why on the
count line instead.

**Which skill is a fact about the ticket**, from its type and its stage:

| type | stage | launches |
| --- | --- | --- |
| `wayfinder:build` | ready · building · needs attention | `claude "/tdd <n>"` |
| `wayfinder:build` | in review | `claude "/review <n>"` |
| research · prototype · grilling · task | any unfinished stage | `claude "/wayfinder <map> <n>"` |
| any | done | nothing — not launchable |

The mode rides whichever route you got, as ` defer`, ` defer: <text>` or
` steer: <text>` on the end of the prompt.

| key | what it does |
| --- | --- |
| *type anything* | fuzzy-filter: the tree flattens to one score-ordered list; clearing restores it |
| `tab` | toggle the leverage view ⇄ the structure forest |
| `↑`/`↓`, `ctrl-j`/`ctrl-k` | move between siblings at the cursor's depth — on the default screen, the tickets you can take (cluster headers are never a stop) |
| `→` | reveal: open a `▸ done`/`▸ blocked` group, else step forward one stop — which *is* descending, since a subtree's first row follows its parent |
| `←` | close: shut an open group, else back out to the parent, else one stop back |
| `enter` | open the launch line on the cursor's ticket; a second `enter` runs the agent here and exits — on a group line it folds instead, since there is no agent to run |
| *type, then `enter`* | on the launch line: the mode (`defer`, `defer <text>`, or steering text) — `esc` backs out with the query and cursor intact |
| `ctrl-f` | focus the cursor row's project — only its clusters stay on screen |
| `ctrl-g` | widen back to every project |
| `ctrl-r` | refetch every map in place, keeping your query, scope and cursor |
| `esc` | clear the query; on an empty query, quit |
| `q` | quit — on an empty query only, since mid-query it types |
| `ctrl-c` | quit from anywhere, including the which-checkout picker |
| `↑`/`↓` or `j`/`k`, `enter`, `esc`/`q` | in the which-checkout picker: pick which tree the agent runs in, or cancel |

`ctrl-r` is the only thing that updates the list in place. Quitting and running
`wf` again is nearly as fast (~0.6 s warm) but throws away the query, the scope
and where the cursor was, which is the whole reason the key still exists now
that nothing polls.

That picker is the one prompt left, and only a repo with several registered
checkouts (the `~/k1/kinisi_ros`, `~/k2/kinisi_ros` pattern) ever sees it: the
agent has to run in exactly one tree and `wf` cannot guess which. One checkout
launches straight away.

`--dangerously-skip-permissions` is passed because the agent is started from a
picker rather than from a shell you are already watching, and stopping on a
permission prompt at that moment would just be a stall you did not ask for.

Going back means running `wf` again, which is cheap — a warm start is ~0.6 s.

### Working while you are away

There is no `ctrl-a`, no headless mode and no auto-start. An agent working
unattended is **another terminal session**: open one, run `wf`, pick the ticket,
switch away. That needs no feature, and `wf` supervising it was the thing that
made `wf` complicated.

Earlier versions ran each ticket in its own `zellij` tab, kept a live poll going
so the frontier stayed fresh, and started `research` tickets by themselves. All
of it is gone as of 0.3.0 — see
[#26](https://github.com/blooop/wayfinder/issues/26) for why and
[#34](https://github.com/blooop/wayfinder/issues/34) for the deletion. `wf` no
longer owns a pty, a session or a background loop; it draws a list, you pick,
and it hands the terminal over.
