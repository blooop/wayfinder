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
carrying the per-status counts (`○` frontier `◐` claimed `⊘` blocked `●` done),
with the map's tickets grouped beneath it. A repo can keep several maps open at
once and each gets its own cluster; focusing a project shows all of them.

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
"/wayfinder <map> <n>"`, with that project's checkout as its cwd — and that is
the end of `wf`. It restores the terminal and **replaces its own process** with
the agent: no picker underneath, no parent waiting, nothing to detach from. The
agent holds the terminal, the exit code and the signals directly, because by
then it *is* the process you started.

| key | what it does |
| --- | --- |
| *type anything* | fuzzy-filter the list; group headers stay, showing `matched/total` |
| `↑`/`↓`, `ctrl-j`/`ctrl-k` | move the cursor over ticket rows (headers are never a stop) |
| `enter` | run the agent on the ticket under the cursor, here, and exit |
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
