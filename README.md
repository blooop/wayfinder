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
nothing else — there is no multiplexer to install. Add
[`dl`](https://github.com/blooop/devlaunch) if you want the agent to run inside
your repos' devcontainers ([Isolation](#isolation-the-agent-runs-in-the-repos-devcontainer));
without it every launch runs on the host, which is what `wf` has always done.

Then link the skills it launches into `~/.claude/skills`:

```
wf skills install
```

`wf --version`, `wf --help` and `wf skills [install]` are the only arguments;
everything else is keys in the TUI.

## The skills ship with the binary

`wf` does not merely *mention* `/wf-tdd` and `/wf-auto` — it hardcodes them
in its routing table and execs them, which makes those prompt files part of
`wf`'s interface. So they live in this repo under `skills/`, the package
installs them at `<prefix>/share/wf/skills`, and one `pixi global update wf`
moves the binary and the prompts together. An interface split across two repos
on two release cadences drifts silently, which is exactly what happened before
this: `wf` reached 0.6.0 still routing `defer` at a `/wf` section that
`/wf-auto` had superseded weeks earlier, and nothing anywhere could
notice.

Five skills ship: `wf`, `wf-auto`, `wf-one`, `wf-tdd` and `wf-review` — the four
`wf` can exec, plus the single-ticket sibling that shares their
`GITHUB_TRACKER.md` and `LIFECYCLE.md`. A unit test asserts every route's label
is one of them, so a route can never name a skill the package does not ship.

Every name carries the `wf` prefix because `~/.claude/skills` is one flat
namespace shared with every other source of skills you have. Unprefixed, `tdd`
and `review` are names `wf` would *squat on* rather than merely occupy: while it
held one, you could not have your own. `wf skills install` clears the links an
older `wf` left under its old names, and touches nothing else — it removes a
link only when the link points into a `wf` bundle, so a skill of yours can never
match however dead it looks.

`wf skills install` **symlinks** rather than copies, which is the whole point:
updating the package updates the prompts, with no second command to remember
and no copy to go stale in between. `wf skills` reports which prompt each route
would actually run:

```
bundle  /home/you/.pixi/envs/wf/share/wf/skills (installed beside the binary)
target  /home/you/.claude/skills

  wf              ok
  wf-auto         ok
  wf-one          ok
  wf-tdd          stale — links to /home/you/projects/wayfinder/skills
  wf-review       not a link — another tool owns this one
```

`wf` never deletes a real directory it did not create — if chezmoi or a
hand-edit owns one, it says so and leaves it, and the other four still install.
Set `WF_SKILLS_DIR` to link a checkout instead of the package, which is what
you want while you are editing the prompts: the link points at your working
tree, so an edit is live in the next session with nothing to reinstall.

The skills stay ordinary installed skills rather than text `wf` injects at exec
time, because `wf` is not their only caller: `LIFECYCLE.md` has the manager
agent spawn `/wf-tdd` and `/wf-review` in *fresh subagents* mid-session, you type
`/wf-auto` yourself on efforts that never go through the picker, and
model invocation needs a file on disk with frontmatter.

## Checks

`.github/workflows/ci.yml` runs four things on every pull request and every push
to main, in the order they are cheap to fix:

```
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked
cargo test --locked --lib --bins --examples
cargo test --locked --test live_fetch -- common::
cargo doc --no-deps --all-features --locked
```

Run them locally with the same commands. CI adds `RUSTFLAGS=-D warnings`, so
anything that warns there fails; the lint *levels* live in one place,
Cargo.toml's `[lints]` table, which is `clippy::all` + `clippy::pedantic` plus a
handful of rustc lints, and denies `unsafe` outright — the only `unsafe` in the
repo is the pty harness in `tests/live_launch_exec.rs`, which says so with a
file-level `allow` and a reason. `rustfmt.toml` and `clippy.toml` hold the rest;
every lint that is switched off carries a comment saying why, so the list stays
arguable rather than accumulating.

Everything under `tests/` is a `live_*` integration test — a real `gh`, real
network, real assertions against this project's own tracker — so CI compiles
them but does not run them, and a moving map never breaks a build. The one
exception is the fourth command above: `tests/common`'s diagnostic, which
decides what a failing live test tells you about a missing `GH_TOKEN`, is pure
and needs no network, and an assertion nothing runs is not an assertion. Run
the rest yourself when the fetch or launch path changes:

```
cargo test --test live_fetch --test live_discovery
```

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
carrying the map's **stage** counts, in the same glyph vocabulary the rows
below it use (`○ ◐ ◍ ! ● ⊘`, described under the leading glyph). A stage the
map has nobody in is left out rather than shown as a zero. A repo can keep
several maps open at once and each gets its own cluster; focusing a project
shows all of them.

**Clusters are ordered by activity, with finished maps last**: the map issue
touched most recently sits at the top, and a map whose every ticket is done
sinks to the bottom however fresh it is — it is history, not work. Maps with
equal (or unreadable) timestamps fall back to `repo`, then map number, so the
order never shuffles between frames.

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
cluster headers and the prompt, green/yellow/red the stage glyphs and counts,
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
leaves the body entirely, counted on the count line as `· N idle maps hidden`.

**A row that heads a branch carries the same rollup of what is beneath it**,
written `(beneath) ⊘2` at the end of the line — so a takeable ticket says the
shape of what taking it unlocks without your reading the subtree. A node is
counted once however many times the tree drew it: the leverage view draws a
dependent under every root that unblocks it, so a diamond in the DAG genuinely
renders one ticket twice inside a single branch.

**`tab` shows the structure forest** instead: the whole blocking DAG, done
tickets dimmed in place. A ticket's tree parent is its lowest-numbered in-map
blocker; edges the tree cannot show are annotated `⤷ also needs #n`.

**Typing sifts**: a live query prunes whichever screen `tab` had toggled down to
the rows that matched, keeping the tree they sit in; clearing it restores that
screen whole. The `>` prompt sits at the **top**, directly under the title, with
the count line under it and the rows beneath both — you type at the top of the
screen and watch the tree sift below it, as `fzf --reverse` does. Only the key
hints stay anchored to the bottom. Matching is fuzzy but **tight** — every character the query lands
on has to start a word or sit against another matched one, so `map` finds "the
**m**anager-**a**gent **p**rotocol" and sub`tree` matches mid-word, while
letters picked out of the middles of three unrelated words do not: `tree`
matched 23 of 30 real tickets under plain fuzzy matching and matches 3 under
this rule. The cost is that an abbreviation skipping *inside* a word — `wf` for
"wayfinder" — no longer matches; `wayf` does. What survives with a match is the cluster header (which project)
and the branch root it hangs from (which takeable ticket unlocks it) — the two
things a hit cannot say about itself. Everything between them is chain length
and elides, leaving a `⋯` in the furniture (`├⋯`) where levels went missing; an
unmatched row that *forks* keeps its line, because two matches need something to
hang from. Rows kept only to place a match are dimmed whole and cannot be landed
on: the cursor still visits nothing but hits, in best-score-first order, so
`↓ ↓ enter` works exactly as it did over a flat list. **The characters the query
actually landed on are underlined** where they were matched — in the row, or in
the cluster header's repo name, since typing a project name is a match on the
one part of the haystack no row draws. A query reaches inside the
collapsed groups too — a done ticket stays findable, and the group opens onto
its matches alone, saying `▾ ● 1 of 5 done`. Clusters with nothing matching
leave the body, header and all.

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
the process you started. A checkout that declares a devcontainer runs that same
agent inside it — see
[Isolation](#isolation-the-agent-runs-in-the-repos-devcontainer).

**Launching is two steps.** `enter` on the cursor's node does not launch: it
opens the **launch line** where the count line was, showing where `enter` will
go — `→ /wf-tdd · #65 Author the /wf-tdd skill`. What you type lands on that line, not
in the query, and *is* the mode:

| the line says | what launches |
| --- | --- |
| *(empty)* | interactive — the default |
| `auto` | the agent decides alone and drives the rest of the lifecycle unattended |
| `auto <text>` | the same, with `<text>` as the steering prompt |
| *anything else* | interactive, with what you typed as the steering prompt |

The line re-resolves as you type, so `auto` visibly flips the skill it names
before you commit to it. `esc` backs out to the list with your query and cursor
exactly as they were. `enter` on a **done** or **blocked** node opens nothing —
it says why on the count line instead.

**The cursor lands on cluster headers too**, so a whole map is a thing you can
launch: `enter` on one runs the wayfinder skill on the map itself rather than on
any one ticket — interactively to chart it with you, or under `auto` to have the
agent take the map from open questions to merged work on its own judgement. The
default cursor position still skips headers and lands on the first row, so
opening `wf` and pressing `enter` picks a ticket exactly as it always did;
headers are one `↑` away.

**Which skill runs is a fact about what you picked and who decides:**

| picked | stage | mode | launches |
| --- | --- | --- | --- |
| a cluster header | — | interactive | `claude "/wf <map>"` |
| `wayfinder:build` | ready · building · needs attention | interactive | `claude "/wf-tdd <n>"` |
| `wayfinder:build` | in review | interactive | `claude "/wf-review <n>"` |
| research · prototype · grilling · task | any unfinished stage | interactive | `claude "/wf <map> <n>"` |
| anything | any unfinished stage | `auto` | `claude "/wf-auto <map> [<n>]"` |
| a ticket | done | — | nothing — not launchable |

`auto` collapses the ticket rows on purpose: the launched session is a
*manager*, and what it manages is the node's whole remaining lifecycle — `/wf-tdd`,
the gate, then a fresh-context `/wf-review` — so it is the manager skill that runs,
not the one skill that stage would have called. Steering text rides whichever
route you got, as ` steer: <text>` on the end of the prompt; the mode itself
never does, because it has already chosen the skill.

| key | what it does |
| --- | --- |
| *type anything* | fuzzy-filter: the tree is pruned to the matches, best-first, with the rows that place them dimmed; clearing restores it |
| `tab` | toggle the leverage view ⇄ the structure forest |
| `↑`/`↓`, `ctrl-j`/`ctrl-k` | move between siblings at the cursor's depth — on the default screen, the tickets you can take, plus each cluster's header above them |
| `→` | reveal: open a `▸ done`/`▸ blocked` group, else step forward one stop — which *is* descending, since a subtree's first row follows its parent |
| `←` | close: shut an open group, else back out to the parent, else one stop back — which, from a cluster's first row, is that cluster's header |
| `enter` | open the launch line on the cursor's ticket, or on its map when the cursor is on a cluster header; a second `enter` runs the agent here and exits — on a group line it folds instead, since there is no agent to run |
| *type, then `enter`* | on the launch line: the mode (`auto`, `auto <text>`, or steering text) — `esc` backs out with the query and cursor intact |
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

### Isolation: the agent runs in the repo's devcontainer

A checkout that carries a **`.devcontainer/devcontainer.json`** (or a top-level
`.devcontainer.json`) launches its agent *inside* that container, by way of
[`dl`](https://github.com/blooop/devlaunch):

```
dl owner/repo@wayfinder/repo-80 -- 'claude' '--dangerously-skip-permissions' '/wf 67 80'
```

`wf` owns which ticket, which checkout, which skill and which prompt. `dl` owns
the container — building it, reusing it, forwarding your `gh` login into it,
keeping an editor from opening over the terminal. `wf` builds no flags, reads no
`devcontainer.json` and writes none: **the repo's own config is the entire
opt-in**, and there is nothing to configure on the `wf` side.

Each launched node gets a **workspace of its own**:
`owner/repo@wayfinder/<repo>-<n>`, where `n` is the ticket's number (the map's,
for a whole-map session). `dl` creates that branch off the default branch if it
is new, clones it into its own cache, and runs the agent in a container per
branch — so launching five tickets is five branches in five containers,
colliding nowhere, and relaunching a ticket reattaches to the workspace it
already has. The branch is the same `wayfinder/<repo>-<n>` that `/wf-tdd` does
ticket `n`'s work on, so a build agent wakes up already on its work branch and a
review launch opens on the branch the PR was pushed from.

The tree you picked in the checkout picker is **not** the agent's working tree:
its jobs are declaring the devcontainer and hosting non-isolated launches. No
agent mutates your checkout, checks out its branches, or fights a second agent
over its index. (Isolated launches used to run in the picked tree itself, which
meant every launch of a repo shared one tree and one container — serial by
construction.)

Two things have to be true, and otherwise the agent runs on the host exactly as
it always has:

1. the checkout declares one of those two configs, and
2. `dl` is on PATH.

**Use `dl` 0.0.20 or newer.** Launching several tickets at once means several
`dl` processes preparing the same repo's cache at the same moment; 0.0.20 is
where those runs serialize over a per-repo lock instead of racing (the loser
of the old race could delete the winner's half-written clone). The older floor
still matters too: `wf` pins no version and never will — the launch
is one `exec` of a program found on PATH — but older `dl` on devpod 0.26 hands
the agent **no terminal**, and an agent with no terminal decides it was invoked
non-interactively: it prints one answer and exits, so the symptom is a session
that never starts rather than an error. Measured here on devpod 0.26.1:
`devpod ssh --command` (what `dl` ≤ 0.0.12 uses) gives `not a tty`, `TERM=dumb`
even from a real terminal, while 0.0.13's `ssh -t` transport gives `/dev/pts/0`,
`TERM=xterm-256color`. The `GH_TOKEN` forwarding below survives that change of
transport; `.devcontainer/devcontainer.json` records why.

**A missing `dl` degrades rather than refuses.** Plenty of repos carry a
`devcontainer.json` for their editor users, and refusing to launch on a machine
that has never installed `dl` would be a regression for people who never asked
for any of this. The launch notice names what you got — `→ wayfinder#80 in
/data/proj/wayfinder (devlaunch)` — and so does each row of the which-checkout
picker, since two trees of one repo can differ.

A repo whose only configs are **variants**
(`.devcontainer/<name>/devcontainer.json`, no default) runs on the host:
choosing among variants would be `wf` picking a container shape, and it has no
basis to pick. Host access, GPU passthrough and the rest are the repo's
declarations to make — `wf` injects no `runArgs`.

Container lifecycle is entirely `dl`'s: `wf` never stops, removes or rebuilds
one, and could not — it `exec`s and is gone, so there is no `wf` left to observe
an agent exiting. `dl <ws> stop`, `dl <ws> rm` and `dl --ls` are the tools, and
they are yours to run. With a workspace per ticket they accumulate faster than
they used to: a merged ticket's workspace has no further use, and reaping it is
manual — `dl --ls` shows what exists, `dl <ws> rm` removes one.

**This container is not a security boundary, and is not trying to be.** It is
for reproducible dependencies. Your host `~/.claude` is bind-mounted into it
read-write and your `gh` login is forwarded as `GH_TOKEN`, so everything running
in a repo's devcontainer — including a `postCreateCommand` you did not write —
gets both. That is a deliberate trade for zero-friction auth, taken with eyes
open ([#73](https://github.com/blooop/wayfinder/issues/73)); the repos this
exists to serve want `network=host`, X11 and device passthrough anyway, which
gives away as much again. Don't point `wf` at a repo you have not read.

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
