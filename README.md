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
your repos' devcontainers ([Isolation](#isolation-claude-runs-in-the-repos-devcontainer));
without it every launch runs on the host, which is what `wf` has always done.

Then link the skills it launches into both CLIs' skills directories:

```
wf skills install
```

This installs the same bundled workflows under `~/.claude/skills` and
`~/.codex/skills`. The launch picker defaults to Claude Code and can switch to
Codex; each agent therefore finds the route it is given without a separate
manual install.

`wf --version`, `wf --help`, `wf skills [install]` and
[`wf reap [-y] [-f]`](#wf-reap--clearing-away-finished-tickets) are the only
arguments; everything else is keys in the TUI.

## The skills ship with the binary

`wf` does not merely *mention* the `wf-tdd` and `wf-auto` skills — it hardcodes
them in its routing table and execs them, which makes those prompt files part
of `wf`'s interface. So they live in this repo under `skills/`, the package
installs them at `<prefix>/share/wf/skills`, and one `pixi global update wf`
moves the binary and the prompts together. An interface split across two repos
on two release cadences drifts silently, which is exactly what happened before
this: `wf` reached 0.6.0 still routing `defer` at a `/wf` section that
`/wf-auto` had superseded weeks earlier, and nothing anywhere could
notice.

Five skills ship: `wf`, `wf-auto`, `wf-one`, `wf-tdd` and `wf-review` — the four
`wf` can exec, plus the single-ticket sibling that shares their
`GITHUB_TRACKER.md` and `LIFECYCLE.md`. A unit test asserts every route's skill
name is one of them, so a route can never name a skill the package does not
ship.

Every name carries the `wf` prefix because both `~/.claude/skills` and
`~/.codex/skills` are flat namespaces shared with every other source of skills
you have. Unprefixed, `tdd` and `review` are names `wf` would *squat on* rather
than merely occupy: while it held one, you could not have your own. `wf skills
install` clears the links an older `wf` left under its old names, and touches
nothing else — it removes a link only when the link points into a `wf` bundle,
so a skill of yours can never match however dead it looks.

`wf skills install` **symlinks**, so a name under either skills directory is
`wf`'s only when `wf` put it there and a real directory can go on meaning
*somebody else owns this*. What the links point at is the part worth knowing:
`~/.claude/wf-skills` and `~/.codex/wf-skills`, **copies** of the package's
bundle kept beside their links, reached *relatively* —
`wf-tdd -> ../wf-skills/wf-tdd`.

That indirection exists for containers. An isolated Claude launch
([Isolation](#isolation-claude-runs-in-the-repos-devcontainer)) mounts your
`~/.claude` into the devcontainer and nothing else: no pixi prefix, and your
home at a different path inside (`/home/vscode`) than out. A link into
`~/.pixi/envs/wf/share/wf/skills` is a fine link on the host and a dangling one
in there. A relative link into a copy that rides the same mount resolves on both
sides. Codex launches on the host until `dl` can hand its `~/.codex` directory
into the container too.

The copy is then the thing that could go stale, so it is not left to trust:
`wf skills install` rewrites them, **every launch brings its selected agent's
copy back in step** before exec'ing, and `wf skills` reports a copy that is not
this build's rather than running a release behind:

```
Claude
bundle  /home/you/.pixi/envs/wf/share/wf/skills (installed beside the binary)
links   /home/you/.claude/skills
copy    /home/you/.claude/wf-skills (what the links point at)

  wf              ok
  wf-auto         ok
  wf-one          outdated — the copy is not this build's
  wf-tdd          stale — links to /home/you/projects/wayfinder/skills
  wf-review       not a link — another tool owns this one

Codex
bundle  /home/you/.pixi/envs/wf/share/wf/skills (installed beside the binary)
links   /home/you/.codex/skills
copy    /home/you/.codex/wf-skills (what the links point at)
```

`wf` never deletes a real directory it did not create — if chezmoi or a
hand-edit owns one, it says so and leaves it, and the other four still install.
Set `WF_SKILLS_DIR` to install a checkout's prompts instead of the package's,
which is what you want while you are editing them: the copy **remembers where it
was made from**, so every launch re-copies from your working tree and an edit is
live in the next session — and an ordinary launch by the released `wf` refreshes
your checkout's prompts rather than quietly replacing them with its own.

The skills stay ordinary installed skills rather than text `wf` injects at exec
time, because `wf` is not their only caller: `LIFECYCLE.md` has the manager
agent spawn `wf-tdd` and `wf-review` in *fresh subagents* mid-session, you type
the appropriate agent's `wf-auto` invocation yourself on efforts that never go
through the picker, and model invocation needs a file on disk with frontmatter.

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
checkouts stay cached but hidden — except the one you are *focused* on, which
renders a slim `no map` header so its first map has somewhere to be started from
(see [Launching](#launching)).

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

Picking a ticket runs the agent chosen in the launch picker, with that project's
checkout as its cwd — and that is the end of `wf`. It restores the terminal and
**replaces its own process** with the agent: no picker underneath, no parent
waiting, nothing to detach from. The agent holds the terminal, the exit code and
the signals directly, because by then it *is* the process you started. A checkout
that declares a devcontainer runs a Claude launch inside it — see
[Isolation](#isolation-claude-runs-in-the-repos-devcontainer).

**Launching is two steps.** `enter` on the cursor's node does not launch: it
opens the **launch picker** over the list, showing what is about to happen and
the two things still undecided — which agent runs it, and who resolves the node:

```
┌ launch Claude · blooop/wayfinder · #65 Author the wf-tdd skill ───────┐
│                                                                         │
│  ▶ interactive   /wf-tdd   you are in the loop; it grills you           │
│    auto          /wf-auto  the agent decides alone and drives it to done│
│    plain         claude    no skill; a bare session on the node's branch│
│                                                                         │
│    steer  █                                                             │
│                                                                         │
│  enter launch · ←/→ agent · ↑/↓ pick · type to fill · esc cancel       │
└─────────────────────────────────────────────────────────────────────────┘
```

`←`/`→` switch Claude and Codex; the selected agent is named in the title and
the route column changes between Claude's `/wf` and Codex's `$wf` syntax.
`↑`/`↓` (or `tab`) move between the rows and `enter` runs that combination, so
the common case is still `enter enter`. Every row is on screen with the skill
it routes *this* node to, because that difference is the choice being made: the
picker is where you see that `auto` means `wf-auto` and will not stop to ask.

Anything you type goes into the field below the rows — on a launch row that is a
**steering prompt** on whichever mode is selected, never a mode itself. That is
the difference from the launch *line* this replaced: the modes were words you had
to already know (`defer`, then `auto`), typing one was indistinguishable from a
typo until the agent ran, and `automate the release` had to be special-cased into
not meaning unattended. No string moves the cursor now, so a launch goes
unattended only because you selected it.

`esc` backs out to the list with your query and cursor exactly as they were.
`enter` on a **done** or **blocked** node opens nothing — it says why on the
count line instead.

**The cursor lands on cluster headers too**, so a whole map is a thing you can
launch: `enter` on one runs the wayfinder skill on the map itself rather than on
any one ticket — interactively to chart it with you, or under auto to have the
agent take the map from open questions to merged work on its own judgement. The
default cursor position still skips headers and lands on the first row, so
opening `wf` and pressing `enter` picks a ticket exactly as it always did;
headers are one `↑` away.

**Starting something new is more rows in the same picker.** Creation is an act on
a *repo*, so it lives where the stop is repo-level — the cluster header — and is
reached by the same `enter` as everything else. No new keys:

```
┌ launch blooop/wayfinder · #59 Map: the dev-process tree ────────────────┐
│                                                                         │
│  ▶ interactive   /wf       you are in the loop; it grills you           │
│    auto          /wf-auto  the agent decides alone and drives it to done│
│    plain         claude    no skill; a bare session on the node's branch│
│    new task      /wf-one   one tracked ticket, built and reviewed       │
│    new map       /wf       chart a new map in this repo, with you       │
│    new map, auto /wf-auto  chart a new map in this repo, alone          │
│                                                                         │
│    task   █                                                             │
│                                                                         │
│  enter launch · ↑/↓ pick · type to fill · esc cancel                    │
└─────────────────────────────────────────────────────────────────────────┘
```

The repo comes free from wherever the cursor was standing, which is why the title
names it. **Ticket pickers are unchanged** — they list the three modes and
nothing else, so `enter` on a ticket never offers to start something instead.

The text field keeps one keyboard behaviour and takes its meaning from the row,
which is what the name beside it says: `steer` on a launch row, `task` on **new
task** — where the text *is* the ticket, so `enter` with nothing typed refuses on
the count line — and `seed` on the two **new map** rows, where it is an optional
loose idea the charting session will grill you about anyway.

Adding a ticket to an existing map needs no row of its own: `enter` on its
header, type `add a ticket for X`, launch — that is `/wf <map> steer: …`, and the
charting session files it.

**A repo with no map has a door too.** Run `wf` inside a registered checkout
whose repo has no open `wayfinder:map` and the screen used to be empty; now it
renders that repo as one slim header, and `enter` on it opens the creation rows
alone — nothing to launch, so no launch rows. That is where a repo's *first* map
gets charted. It appears only on the focused empty state and only once the load
has landed, so a still-fetching repo never flashes a creation row under `enter`,
and the widened screen stays free of one row per project.

**Which skill runs is a fact about what you picked and who decides:**

| picked | stage | mode | skill |
| --- | --- | --- | --- |
| a cluster header | — | interactive | `<sigil>wf <map>` |
| `wayfinder:build` | ready · building · needs attention | interactive | `<sigil>wf-tdd <n>` |
| `wayfinder:build` | in review | interactive | `<sigil>wf-review <n>` |
| research · prototype · grilling · task | any unfinished stage | interactive | `<sigil>wf <map> <n>` |
| anything | any unfinished stage | auto | `<sigil>wf-auto <map> [<n>]` |
| anything | any unfinished stage | plain | the selected agent, with no skill; anything typed is the whole prompt |
| a cluster header, or a map-less repo | — | new task | `<sigil>wf-one <task>` |
| a cluster header, or a map-less repo | — | new map | `<sigil>wf [<seed>]` |
| a cluster header, or a map-less repo | — | new map, auto | `<sigil>wf-auto [<seed>]` |
| a ticket | done | — | nothing — not launchable |

A creation has no issue number until the skill files one, so it has no per-node
branch either: it runs on the repo's **default workspace**, and the launched
skill makes its own branches from there.

Claude receives that skill prefixed with `/` through
`claude --dangerously-skip-permissions`; Codex receives it prefixed with `$`
through `codex --dangerously-bypass-approvals-and-sandbox`. Both receive one
prompt argument, so the route, numbers, and steering text cannot be split apart
by the shell.

**Every launch of a node also hands the agent what `wf` already knew**, as a
`ctx: <json>` block between the skill's arguments and any steering suffix:

```
/wf-review 124 ctx: {"v":1,"repo":"blooop/wayfinder","map":{…},"aim":{"ticket":{…,"prs":[…]}}} steer: <text>
```

That is the parent map, the ticket's type and stage, and its linked PRs — the
three serial `gh` calls a launched skill used to open with, answered before it
starts. It is an accelerator and never a precondition: a skill invoked by hand
never went through the picker, finds no block, and discovers exactly as before.
The one thing it deliberately cannot say is whether the ticket is still yours to
take — there is no assignee and no ticket status in the schema, so claiming stays
a live call and a stale block cannot make an agent act on someone else's work.
The creation rows carry no block at all: they name nothing that exists yet.

The auto mode collapses the ticket rows on purpose: the launched session is a
*manager*, and what it manages is the node's whole remaining lifecycle —
`wf-tdd`, the gate, then a fresh-context `wf-review` — so it is the manager
skill that runs, not the one skill that stage would have called. Steering text
rides whichever route you got, as ` steer: <text>` on the end of the prompt;
the mode itself never does, because it has already chosen the skill.

**The plain mode collapses them for the opposite reason: it runs no skill at
all.** Everything else about the launch is unchanged — same checkout, same
per-node branch, same clone and container — so it is the way to get `wf`'s
workspace without a skill's opinion about what to do in it: reading the code a
ticket is about, a quick fix too small to be a node's lifecycle, or picking up
after an agent that stopped somewhere awkward. There being no skill in front of
it is also why steering text is handled differently here: a ` steer: <text>`
suffix would be addressed to nobody, so whatever you type is simply the
session's opening prompt, and typing nothing passes no prompt at all rather than
an empty one.

| key | what it does |
| --- | --- |
| *type anything* | fuzzy-filter: the tree is pruned to the matches, best-first, with the rows that place them dimmed; clearing restores it |
| `tab` | toggle the leverage view ⇄ the structure forest |
| `↑`/`↓`, `ctrl-j`/`ctrl-k` | move between siblings at the cursor's depth — on the default screen, the tickets you can take, plus each cluster's header above them |
| `→` | reveal: open a `▸ done`/`▸ blocked` group, else step forward one stop — which *is* descending, since a subtree's first row follows its parent |
| `←` | close: shut an open group, else back out to the parent, else one stop back — which, from a cluster's first row, is that cluster's header |
| `enter` | open the launch picker on the cursor's ticket, or on its map when the cursor is on a cluster header; a second `enter` runs the agent here and exits — on a group line it folds instead, since there is no agent to run |
| `←`/`→`, `↑`/`↓` or `tab`, *type*, then `enter`* | in the launch picker: pick Claude/Codex, pick the row, fill its field (a steering prompt, a task, or a map seed), launch — `esc` backs out with the query and cursor intact |
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

Each agent receives its own explicit permission-bypass flag because it is
started from a picker rather than from a shell you are already watching, and
stopping on a permission prompt at that moment would just be a stall you did
not ask for.

Going back means running `wf` again, which is cheap — a warm start is ~0.6 s.

### Isolation: Claude runs in the repo's devcontainer

A checkout that carries a **`.devcontainer/devcontainer.json`** (or a top-level
`.devcontainer.json`) launches a **Claude** session *inside* that container, by way of
[`dl`](https://github.com/blooop/devlaunch):

```
dl owner/repo@wayfinder/repo-80 -- 'claude' '--dangerously-skip-permissions' '/wf 67 80 ctx: {"v":1,…}'
```

Codex deliberately stays on the host even for such a checkout. `dl` mounts
`~/.claude` but not `~/.codex`, so an isolated Codex session would lose both its
login and its `$wf` skills after `wf` had already handed over the terminal. The
host is the only launch `wf` can make honestly until `dl` gains a Codex handover.

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

**Optional: warm the container at the first enter.** With `WF_PREWARM=1` set,
staging an isolated node fires a background `dl <workspace> up`, so the image
pull, the clone and the tool install run while you are still picking a mode and
typing steer text. By the second enter the container is ready or nearly so —
`dl` serializes the launch against the warm-up (a per-workspace lock), so the
agent attaches to the container the prewarm built instead of racing it. On a
cold node that is most of the wait gone.

It is **off by default**, and the reason is worth reading before you turn it
on. It makes the first enter — until now a keystroke that only opened an
overlay — do real work: it fetches the repo, creates a work branch in `dl`'s
cache, clones it, and builds and starts a container. That is a good trade
while you are working a map and a bad one while you are browsing it.

Two things to be clear about, because the cleanup is not automatic:

- **An abandoned stage leaves its workspace behind.** Back out of the launch
  picker, pick a host checkout at the which-checkout prompt, or pick a
  *creation* row on a map (a creation runs in the repo's bare workspace, not
  the node's), and the branch, the clone and the container stay. `wf reap`
  will *not* collect them while the ticket is open — it only removes
  workspaces whose tickets are **closed** — so until then they are yours to
  remove with `dl <workspace> rm`.

  Only a **node** is ever warmed. The map-less door offers creation rows
  alone, so staging it warms nothing: there is no node for a launch to attach
  to, and a keystroke should not pre-build a repo's default workspace on the
  chance you file something.
- **It does reach the network, but it never publishes.** Fetching the repo and
  pulling the image are the point of doing it early. What it does not do is
  write anything to GitHub: `dl` creates the work branch locally and never
  pushes it, so an abandoned stage leaves no branch, no PR and no comment
  behind.

The tree you picked in the checkout picker is **not** the agent's working tree:
its jobs are declaring the devcontainer and hosting non-isolated launches. No
agent mutates your checkout, checks out its branches, or fights a second agent
over its index. (Isolated launches used to run in the picked tree itself, which
meant every launch of a repo shared one tree and one container — serial by
construction.)

Two things have to be true for a Claude launch to be isolated, and otherwise it
runs on the host exactly as it always has:

1. the checkout declares one of those two configs, and
2. `dl` is on PATH.

**`WF_PREWARM=1` needs `dl` 0.0.24 or newer.** `dl <workspace> up` — start
without attaching — and the per-workspace launch lock are what the warm-up is
made of; on an older `dl` the background spawn fails silently and the launch
simply pays the cold start at the second enter, as it always did.

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

Container lifecycle is `dl`'s: `wf` never stops or rebuilds one, and could not
during a session — it `exec`s and is gone, so there is no `wf` left to observe
an agent exiting. `dl <ws> stop`, `dl <ws> rm` and `dl --ls` are yours to run.

### `wf reap` — clearing away finished tickets

A workspace per ticket means workspaces accumulate as fast as tickets are
worked. `wf reap` removes the ones whose **work is over**:

```
$ wf reap
  keep  devlaunch-wayfinder-devlaunch-131-a  (devlaunch#131 is still open)
  keep  wayfinder-wayfinder-wayfinder-42-b   (still running — stop it first)
  warn  devlaunch-wayfinder-devlaunch-118-d  (devlaunch#118's PR #120 closed unmerged — superseded? reap by hand if so)
  warn  wayfinder-wayfinder-wayfinder-96-e   (wayfinder#96 unclaimed and no PR — an abandoned stage? reap by hand if so)
  reap  devlaunch-wayfinder-devlaunch-127-c  (devlaunch#127 is closed)
  reap  devlaunch-wayfinder-devlaunch-80-f   (devlaunch#80 open but its PR #97 merged)

delete 2 workspace(s)? [y/N]
```

"Over" is not a judgement `wf` invents for the occasion — it is exactly what
the stage lattice already calls Done, read off the same fields
as the `⇄` badge on the screen: a **closed** ticket, or an **open** one whose
PR merged with nothing still in flight. The second case is the one that earns
this its keep, since a ticket often outlives the branch that finished it. And
because the guards run first, an un-`-f`'d reap only ever deletes a checkout
whose every byte exists on the remote: being wrong costs a re-clone, never
work.

`warn` rows are the other end of that lattice, and they are **never deleted** —
no flag makes them so. `wf` prints them because it suspects dead weight on
evidence too weak to act on:

- every linked PR **closed unmerged** — a human's "not this way", which is not
  the same as "this branch is disposable";
- **nobody claimed it and nothing came of it** — no PR, no assignee. An open,
  unassigned ticket is unclaimed by `wf`'s own convention, and that one bit is
  what keeps this from firing on every ticket someone is mid-way through.

A stale claim therefore keeps a workspace: an agent that died leaves its ticket
assigned, and reap does not overrule a person's stated intent. `wf` says only
what it observed — it does not know whether anyone ever entered a container,
and does not claim to.

This is the same division of labour the launch draws: **`dl` owns the
containers, `wf` owns the tickets.** `dl` deliberately does not decide what is
finished — that is a fact about a ticket, and inferring it from the branch
cannot tell a squash-merged branch from an abandoned one — so it publishes what
it knows (`dl --ls --json`) and refuses to destroy work that exists nowhere
else, while `wf`, which minted those branch names from its own ticket numbers,
decides. Needs `dl` **0.0.21 or newer** for the JSON listing.

Kept, always: workspaces `dl` did not create (they are not `wf`'s, whatever
their branch looks like), branches that are not `wayfinder/<repo>-<n>` for that
repo, tickets with an open or draft PR (in review is where review fixes happen),
tickets someone has claimed, and anything **running** — a ticket closing is no
evidence that the session in the container ended.

One `gh api graphql` call per repo answers all of this, however many workspaces
that repo has.

Kept by default but waivable: a workspace whose clone holds uncommitted or
unpushed work. `-f` reaps those too, and the plan says what it is discarding:

```
  reap  devlaunch-…-127-c  (devlaunch#127 is closed, discarding 1 uncommitted change(s) (pixi.lock))
```

`-f` exists for a case that is not hypothetical: a devcontainer whose
`postCreateCommand` installs packages leaves a tracked lockfile modified in
*every* workspace it builds, so without it those are unreapable forever. It
waives the unsaved-work guard only — a running container is still kept, because
that is a session in progress rather than bytes on disk. `-y` skips the prompt.
Neither flag can turn a `warn` row into a deletion.

Your host `~/.claude` is bind-mounted in, which is how the Claude agent arrives
already logged in — and is the reason `wf skills install` keeps the prompts
*inside* that directory and links to them relatively. `~/.pixi` is not mounted
and your home is at a different path in there, so a link into the package prefix
dangles and the launch answers `Unknown command: /wf-tdd`. See
[the skills](#the-skills-ship-with-the-binary).

**This container is not a security boundary, and is not trying to be.** It is
for reproducible dependencies. That `~/.claude` mount is read-write and your
`gh` login is forwarded as `GH_TOKEN`, so everything running
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
