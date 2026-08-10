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

## Two levels: projects, then one project

`wf` has exactly two screens, and which one it opens on is decided by where you
ran it.

**Inside a checkout** it opens on **that project**: its own row first, then its
maps as clusters beneath.

```
▶ ▌ blooop/wayfinder · 2 maps  ○4  ◐2  ◍1

  ▌ wayfinder · the general dev-process tree  ○2  ◐1
    ○ #134 The manager hands pointers, never readings [build]
    ○ #133 The ctx handoff's guards don't yet guard   [build]
```

**Outside one** it opens on the **project list** — every registered repo, most
recently used first — and `enter` (or `→`) on a row goes to exactly the screen
above. One project view, two doors into it.

```
▶ ▌ blooop/wayfinder · 2 maps  ○4  ◐2  ◍1
  ▌ blooop/devlaunch · 1 map   ○2  ●7
  ▌ blooop/bencher   · no map — enter to start one
```

`←` is the way back out, from anywhere: it closes a group, climbs to the parent,
steps back a stop, and from the project's own row leaves for the list. Held
down it walks you from a ticket to the top level. `esc` is deliberately not
this — it still clears the query and quits, so leaving `wf` never depends on how
deep you are.

This is what retired `ctrl-f` (focus) and `ctrl-g` (widen): focusing a project
is entering it, widening is `←` out of it, and both chords named a move the
arrows already make. They are unbound, not merely undocumented.

There is no screen that pours every project's tickets into one tree. That was
the old widened scope, and the trade is deliberate: the query means projects at
the top level and tickets inside one, instead of a single field matching both.
The cost is that a ticket in a project you have not thought of is not findable
by typing its title.

**The project list needs no network.** It is the projects cache, ordered by a
local timestamp, so it is on screen and answering keys before the first `gh`
call — which is also what lets `enter`, type, `enter` file a task in a fresh
checkout before the map search has returned. The counts on each row fill in as
the maps land; a row reads `loading…` until the search has answered and `no
map — enter to start one` once it has.

## Discovery

Projects accrete: running `wf` inside a git checkout with a GitHub `origin`
registers that checkout.

The registry is a per-machine cache at `~/.cache/wf/projects.json`
(`$XDG_CACHE_HOME` respected) holding `{path, repo, used}` per checkout, plus
every open map that the last search found and a `{repo, number, agent,
checkout, at}` session per node you have launched an agent on (see
[Resuming](#resuming-picking-a-conversation-back-up)). Deleting it is safe —
projects re-accrete as you open them, the maps are re-found on the next start,
and the only thing actually lost is the offer to rejoin conversations, which
still exist where their agents left them.

`used` is what orders the project list: a local stamp, written when you open
`wf` in a checkout and when you launch an agent from one. A *local* stamp, and
not the tracker's activity, because the list is drawn before any fetch — and
because "which project did the world touch last" and "which one did **you**"
are different questions, and a launcher is answering the second. Entries written
before this existed carry no stamp and sort last, once, until you next open
them.

A repo with several checkouts (the `~/k1/kinisi_ros`, `~/k2/kinisi_ros`
pattern) is **one** project: they are two places it can run, and they share its
maps, so the project's stamp is the newest of theirs.

Every registered repo is a row on the project list, mapped or not. A repo with
no open `wayfinder:map` simply has no clusters on its screen — its own row is
still there, and is where its first map gets charted (see
[Launching](#launching)).

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

### What is actually running, and what stopped

Every glyph above is a **tracker** fact. That is the right vocabulary for what
the work *is*, and it is silent about the half `wf` itself created: a launch
`exec`s an agent into a container and exits, so nothing has ever reported back
whether that agent is still there. `⏎` comes closest and deliberately stops
short — it says a launch *happened*.

`dl` knows the missing half and already publishes it, so the same background
reading that finds [reclaimable workspaces](#startup) reads it too, and two
markings come out of the join:

- **`▣` a container of this node's is up.**
- **`⧖` claimed, nothing pushed, and nothing of its running.** Somebody — almost
  always an agent — took this ticket and is no longer on it.

`⧖` is the one that needed both halves, and it is why this is a join rather than
a field. A claim on its own is the ordinary look of work in progress; a stopped
container on its own is the ordinary look of work that finished. It is the
*pair* that means a lifecycle went down between its stages. It reads against the
stage glyph beside it, and that contrast is the finding: `◐ #133 … ⧖` is the
tracker saying *building* and the machine saying *nothing is*.

**These markings are taken once, at startup, and never again** — and that is a
real limitation rather than an oversight. The reading is taken behind the first
frame and there is no key that retakes it, so `▣` and `⧖` say what was true when
`wf` started, not what is true now. Start a container from the launch picker of
this very session and the row behind it will not learn: the marking you are
looking at was decided before you pressed anything.

They stay useful because of how short a session is — `wf` is on screen for
seconds — and because the thing they report moves in minutes rather than
milliseconds. But a screen you left open while a build ran is a screen making
claims in the past tense, and the only way to bring them up to date is to quit
and run `wf` again, which is ~0.6 s warm.

Stalls also reach the [count line](#startup) as
`· 2 stalled: wayfinder#133, wayfinder#134`, because the row is not always on
screen — the project list has no
ticket rows at all, and a stall can be inside a fold or another map. Running
containers stay on their rows: they are the ordinary state of a machine in use,
and a count of them would be a status bar rather than a summons.

Four things this deliberately does not claim, because the honest version is
narrower than the useful-sounding one:

- **A container being up is not an agent being alive.** `dl` reports the
  container; nobody reports the process inside it, and `wf` is long gone by
  then. `▣` covers a session you left, a session that exited an hour ago inside
  a container nobody stopped, and a `WF_PREWARM` container never entered. It is
  a floor on activity, not a reading of it.
- **A stall is not a crash**, and a reboot marks everything at once. The same
  shape is left by an agent that died mid-slice, one that handed off cleanly, a
  `dl <ws> stop` you ran yourself, and a restart that stopped every container on
  the machine. None of those is a false positive — each of those runs really has
  stopped and really does want picking up — but they arrive together, so
  `12 stalled` the morning after a reboot is a fact about the host rather than
  about the work. The breadcrumb trail on the ticket says which.
- **A node launched on the host can still be marked**, and this is the one
  outright wrong answer. `wf` cannot see host processes, so a node whose agent
  is running on the host, but which owns a stopped workspace from some *other*
  launch — the repo grew a `.devcontainer/` later, or `WF_PREWARM` built one at
  a staging you backed out of — looks exactly like a stall. A node with no
  workspace at all is genuinely unmarked rather than wrongly marked.
- **This machine only**, the same limit [resume](#resuming-picking-a-conversation-back-up)
  carries: the listing is local, so a ticket worked on another machine looks
  unstarted here.

Nothing is done about any of it. `wf reap` still keeps a claimed workspace — a
stale claim is a person's stated intent and reap does not overrule it — which is
exactly why the stall was worth drawing: it was the one thing on the machine
that nothing pointed at.

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
fetching the maps themselves immediately — real tickets in ~0.6 s. Every map is
fetched concurrently, so N projects cost one round trip rather than N.

The cache is a head start, never a skip: the search still runs on every start,
and its answer is what adds a repo mapped since last time, drops one whose map
was closed, and corrects a number that moved. A cached number is never taken at
its word either — a map fetch checks the issue is still an open `wayfinder:map`
and refuses it otherwise, so a stale number renders *nothing* for a moment
rather than the wrong issue as that project's map. A search that fails is
retried rather than fatal. Which project's screen is up is not the search's
business either way: it comes from the local `git` call that registers the
checkout, before the first frame, and nothing arriving later moves it.

Each map is fetched exactly once per run, and there is no key that asks again.
Nothing polls either: `wf` is on screen for seconds and restarts warm in ~0.6 s,
so a whole new run is the refresh — at the price of the query, the project you
had entered and where the cursor was. A fetch that fails says so on the count
line and stays failed for the rest of the run; the screen says `run wf again`
rather than naming a key, because running it again is what retries.

**What a `wf reap` would claim is read in the background too**, and lands on the
count line when it arrives:

```
  12/30  · 1 stalled: wayfinder#133 · 2 reclaimable: devlaunch-github-…oop-wayfinder-127, +1 more — wf reap
```

That one reading answers two questions, because the same `dl --ls --json` and
the same tracker query settle both: what a reap would claim, and
[what is running and what stopped](#what-is-actually-running-and-what-stopped).
Stalls are laid down first, so on a narrowing terminal it is the `wf reap`
pointer and the warned aside that go while stalls are still naming nodes — the
one trade in here worth disagreeing with: work that has stopped moving outranks
tidying that can wait. What stalls will not do is squeeze the reclaim note below
its own count, because that segment clips rather than vanishing and `· 2
reclaima` is not a word; they give up a name instead, which the rows' own `⧖`
markings make readable anyway.

It is the same reading `wf reap` prints, taken by the same code — a `dl --ls
--json` and one batched tracker query, neither of them on the way to a frame.
The picker draws at exactly the speed it did without it. Nothing is deleted:
this names workspaces and a command, and you type the command. It names them
rather than only counting them, because "2 reclaimable" is not something you can
agree or disagree with.

**This path does not delete workspaces**, and it is worth being exact about what
holds that up, because it is three different things with three different
reaches — and only the first of them is a proof.

The function that removes a workspace is private to the library module that owns
`wf reap`. The picker is in the binary, so no line of it — no helper, alias or
submodule — can call that function, and the edit that tries does not compile.
That one is settled by the compiler and needs no test.

`wf reap` as a whole *command* is another matter: it has to be public for `main`
to dispatch it, and `reap::run(true, true)` is a forced reap. So the picker file
may not write the word `reap` at all — nor `Command`, `process::`,
`tokio::spawn` or `fs` — and `main.rs` carries the list of every line of code in
itself that writes `reap`, and may not write `Command` or `fs` either. Words,
not paths: `use std::fs as sys;` writes no `fs::` and `use crate::reap as tidy;`
writes no `reap::`, and both of those were live escapes against the narrower
spelling. `main.rs`'s list is of lines of *code* — its `USAGE` help text writes
`wf reap` because that is the command it documents, and it is cut out before the
list is taken, with the cut checked against `USAGE`'s own line count so it
cannot run past the literal and swallow code. Those are greps over the source
text of two files. They catch a cleanup wired in by accident, which is the
mistake anyone is actually likely to make; they do not stop someone who means to
get around them, and several review rounds of this feature were spent proving
exactly that. A grep over two files cannot see the same call written in a third,
or a second name for the module exported from the library.

Everything else is watched rather than proven. A test drives the real event loop
against a recording `dl` and `gh`, through every arm the loop has — quit,
continue, and the launch that ends the session — and reads back every argv the
run made, plus a scratch `HOME` laid out the way this machine is
(`~/.cache/devlaunch/repos/<owner>/<repo>/<id>` for the clone,
`~/.devpod/contexts/<ctx>/workspaces/<id>` for the record), compared before and
after. That catches a workspace deleted by running `dl` and one deleted
in-process, whatever file the call was written in, inside three stated bounds.
It starts at the loop's composition — the function that spawns the reading and
runs the loop — so what `wf` does above and below that call is outside it; what
stands there instead is the token list above, in those two files and no others.
It runs for the length of that call plus 400 ms after the reading lands, after
every key and after it
returns, so a deletion deferred past the window is not seen (a spawned task that
sleeps a second and a half is green; the same one with the window at three
seconds is caught). And it sees only what the child's fixtures reach: a deletion
in the one fold arm a failed map search would take is green, because the `gh`
shim succeeds. An `std::fs` call is caught by nothing when it is aimed outside
that scratch `HOME` **or written outside the span the run covers** — and that
span is the loop's composition, so "outside" it is the whole of the picker's own
entry point and the whole of `main`. A `remove_dir_all` pointed at a directory
elsewhere on the disk passes the whole suite and really does destroy it; a
`remove_dir_all` aimed squarely *inside* the watched home, written in either of
those two functions, passed it too until the `fs` token above went on. What
catches the second kind is that token, in the two files that carry it, and it is
a grep. Nothing catches either kind in a third file.

The picker does delete one thing, and it is not a workspace: the launch path
brings the installed skill copies back in step with the bundle, which removes
and rewrites `~/.claude/wf-skills/<skill>` when a copy has fallen behind. That
is `wf`'s copy of `wf`'s own prompts.

A `dl` workspace id is around forty characters and this segment shares one line
with the load state and the match count, so the line is **budgeted** rather than
written and clipped: the count, the `(+N to check by hand)` aside and the
`— wf reap` pointer are laid down first, and the names take what is left. As many
are spelt out whole as fit; the next one is shortened in the middle, keeping the
project at the front and the ticket number at the back; the rest are counted as
`+N more`. On a terminal too narrow for a name anybody could read, the names go
and the command stays; narrower still, the aside goes and the command *still*
stays, because it is the only part of the line you can act on. The segment never
overruns the width it is given at any terminal size.

It **fails silent**. No `dl` on PATH, a listing that failed, a GraphQL error, no
network, or simply nothing to reclaim: the segment is absent, and there is no
error and no delay. A cleanup convenience is not worth a degraded launcher.
Workspaces `wf reap` would *warn* about rather than delete are never counted
into it — they show up only as a `(+N to check by hand)` aside.

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
agent take the map from open questions to merged work on its own judgement.

**The cursor opens on the project's own row**, not on a ticket. This
consciously overrides the older rule that opening `wf` and pressing `enter`
picked the top ticket: with creation on the project row, the default became
`enter`, type what you want to do, `enter` — the thing you most often open `wf`
in a repo to do, with no navigation at all. Picking the top ticket is one `↓`
first. Two things make the trade a good one: an empty `task` field refuses, so
the default cannot misfire; and the project row is known before any fetch, so
unlike the top ticket it never moves under you as maps stream in — which is what
#88 and #89 were both about.

**Starting something new lives on the project row.** Creation is an act on a
*repo*, so it lives on the one stop that is a repo — and it is where the cursor
already is when the screen opens, so `enter`, type, `enter` is the whole of
filing a task in the project you are standing in. No new keys, and no
navigation:

```
┌ launch Claude · blooop/wayfinder · +new ────────────────────────────────┐
│                                                                         │
│  ▶ new task      /wf-one   one tracked ticket, built and reviewed       │
│    new map       /wf       chart a new map in this repo, with you       │
│    new map, auto /wf-auto  chart a new map in this repo, alone          │
│                                                                         │
│    task   █                                                             │
│                                                                         │
│  enter launch · ↑/↓ pick · type to fill · esc cancel                    │
└─────────────────────────────────────────────────────────────────────────┘
```

There is nothing to launch on a project — it names nothing that exists yet — so
there are no launch rows, and `enter` with an empty `task` field refuses on the
count line rather than filing something blank. Which is why the creating default
is safe: `enter enter` on a fresh screen does nothing at all.

### Resuming: picking a conversation back up

A node you have launched an agent on before carries a `⏎` in the list, and its
picker leads with a **resume** row:

```
┌ launch Claude · blooop/wayfinder · #117 The one project surface ────────────┐
│                                                                             │
│  ▶ resume        claude --continue   20m ago · pick the conversation back up│
│    interactive   /wf-tdd             you are in the loop; it grills you     │
│    auto          /wf-auto            the agent decides alone and drives it  │
│    plain         claude              no skill; a bare session on the branch │
│                                                                             │
│    steer  █                                                                 │
│                                                                             │
│  enter launch · ↑/↓ pick · type to fill · esc cancel                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

So coming back to work you left is `enter enter`, the same two keys as starting
it. Starting fresh instead is one `↓`. The row leads for the reason the project
row takes the cursor on the screen behind it: the default should be the likeliest
act, and the alternative should cost one key.

**It needs no session store, because neither agent has one worth storing.**
`claude --continue` continues "the most recent conversation in the current
directory", and `codex resume --last` filters by cwd unless told `--all`. `wf`
already gives every node a working directory of its own — that is exactly what
the per-node workspace `owner/repo@wayfinder/<repo>-<n>` *is* — so going back is
a matter of exec'ing in the same place. No ids are matched, nothing can point at
a conversation that moved, and a resume is the same exec every other launch is
with the agent's own flag in place of a skill.

What `wf` records at each launch is therefore three facts and no more, in the
same `projects.json` the checkouts live in — the three that between them say
*where the conversation is*: **which tree**, **host or container**, and **which
CLI**. The tree is the half a fresh reading cannot recover once you have two
checkouts of one repo, which is why a resume never asks *which checkout* where a
fresh launch would: the conversation exists in exactly one of them and the
record says which. Host-or-container is recorded rather than re-detected because
an isolated launch's history lives inside `dl`'s clone — re-detecting from a
checkout that has since lost its `.devcontainer/` would answer "host" and
quietly resume the checkout's own, different conversation. And the CLI cannot be
re-derived at all, since a Claude conversation is not rejoinable by Codex — so
the picker's `←`/`→` is deliberately dead on this row, and the title names the
recorded agent rather than the picked one.

The workspace name is deliberately *not* recorded: it is a pure function of the
node (`owner/repo@wayfinder/<repo>-<n>`), so a stored copy could only ever
disagree with a fresh derivation.

Two honest limits. The record says **`wf` launched an agent here**, not that a
conversation exists — that is the strongest thing `wf` can know without reading
the agent's own store, which for an isolated launch is inside a container at a
path devpod chose. A launch that died before its agent wrote anything leaves a
resume row that lands on the agent's own "no conversation found". And this is
**same-machine** only, by construction: it is a local cache of local
directories. Coming back on another machine is what the breadcrumbs on the
ticket are for.

**A node launches and a project creates, and neither does the other's job.** A
cluster header is a *map*, so its picker is the three modes and wraps, exactly
like a ticket's — the only difference between the two is what they aim at. The
creation rows used to ride along on the header, on the grounds that a header was
the only repo-level stop there was; a project row is a better one, and having
both would put `new map` on every header of a repo with three maps open — three
doors to one act, none of them the repo.

The text field keeps one keyboard behaviour and takes its meaning from the row,
which is what the name beside it says: `steer` on a launch row, `task` on **new
task** — where the text *is* the ticket — and `seed` on the two **new map** rows,
where it is an optional loose idea the charting session will grill you about
anyway.

Adding a ticket to an existing map needs no row of its own: `enter` on its
header, type `add a ticket for X`, launch — that is `/wf <map> steer: …`, and the
charting session files it.

**A repo with no map needs no special case.** Its screen is its project row and
nothing else, and that row is where its *first* map gets charted. That row is
also there before the search has answered — it is a place to stand, not a report
on what was found — which is what makes the create path work on the first frame.

**Which skill runs is a fact about what you picked and who decides:**

| picked | stage | mode | skill |
| --- | --- | --- | --- |
| a cluster header | — | interactive | `<sigil>wf <map>` |
| `wayfinder:build` | ready · building · needs attention | interactive | `<sigil>wf-tdd <n>` |
| `wayfinder:build` | in review | interactive | `<sigil>wf-review <n>` |
| research · prototype · grilling · task | any unfinished stage | interactive | `<sigil>wf <map> <n>` |
| anything | any unfinished stage | auto | `<sigil>wf-auto <map> [<n>]` |
| anything | any unfinished stage | plain | the selected agent, with no skill; anything typed is the whole prompt |
| a project row | — | new task | `<sigil>wf-one <task>` |
| a project row | — | new map | `<sigil>wf [<seed>]` |
| a project row | — | new map, auto | `<sigil>wf-auto [<seed>]` |
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
| *type anything* | fuzzy-filter, best-first: the project list narrows to matching slugs; a project's screen prunes the tree to the matching tickets, with the rows that place them dimmed. Clearing restores it |
| `tab` | toggle the leverage view ⇄ the structure forest |
| `↑`/`↓`, `ctrl-j`/`ctrl-k` | move between siblings at the cursor's depth — on the default screen, the tickets you can take, plus each cluster's header above them |
| `→` | reveal: enter the project on the project list, else open a `▸ done`/`▸ blocked` group, else step forward one stop — which *is* descending, since a subtree's first row follows its parent |
| `←` | close: from a project's own row, back out to the project list; else shut an open group, back out to the parent, or step one stop back — which, from a cluster's first row, is that cluster's header |
| `enter` | on the project list: enter that project. On a project's screen: open the launch picker — the creation rows on the project's own row, the launch modes on a ticket or a cluster header — and a second `enter` runs the agent here and exits. On a group line it folds instead, since there is no agent to run |
| `←`/`→`, `↑`/`↓` or `tab`, *type*, then `enter`* | in the launch picker: pick Claude/Codex, pick the row, fill its field (a steering prompt, a task, or a map seed), launch — `esc` backs out with the query and cursor intact. `←`/`→` does nothing on a `resume` row, whose agent comes from the record |
| `⏎` in a row | a previous launch left a conversation on this node: its picker leads with `resume`, and `enter enter` rejoins it |
| `▣` in a row | [a container of this node's is up](#what-is-actually-running-and-what-stopped) — which is not the same as an agent being alive in it |
| `⧖` in a row | claimed, nothing pushed, and nothing of its running: a run that stopped between its stages |
| `esc` | clear the query; on an empty query, quit |
| `q` | quit — on an empty query only, since mid-query it types |
| `ctrl-c` | quit from anywhere, including the which-checkout picker |
| `↑`/`↓` or `j`/`k`, `enter`, `esc`/`q` | in the which-checkout picker: pick which tree the agent runs in, or cancel |

**Nothing updates the list in place.** There was a key that refetched every map
without losing your query, level or cursor, and it is gone: it was the only
thing in `wf` that wrote the screen's state twice, and each of those second
writes needed its own machinery to be safe — a load restart so a refetch could
not be beaten by the load it replaced, a way to put the startup counter back
into loading, and a generation tag on the background reading so an answer to a
question you had already withdrawn could be told from a live one. One key's
worth of freshness was not worth a second write path through everything behind
the screen. Quitting and running `wf` again is ~0.6 s warm and costs you the
query, the level and the cursor, and that is the trade as it now stands.

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
  picker, or pick a host checkout at the which-checkout prompt, and the branch,
  the clone and the container stay. `wf reap` will *not* collect them while the
  ticket is open — it only removes workspaces whose tickets are **closed** — so
  until then they are yours to remove with `dl <workspace> rm`.

  Only a **node** is ever warmed, which is now exactly the stops that have one:
  a ticket or a cluster header. Staging a **project** row warms nothing — its
  rows all create, so there is no node for a launch to attach to, and a
  keystroke should not pre-build a repo's default workspace on the chance you
  file something.
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

Three things have to be true for a Claude launch to be isolated, and otherwise
it runs on the host exactly as it always has:

1. the checkout declares one of those two configs,
2. `dl` is on PATH, and
3. that `dl` is **0.0.24 or newer**.

The third is there because "installed" and "speaks this binary's command line"
are different questions, and asking only the first moved the failure past the
point where it could still degrade: the prewarm below fires `dl <workspace> up`,
which no release before 0.0.24 had, so a machine with everything up to date
satisfied both of the old conditions and then failed *inside* the launch. `wf`
asks `dl --version` once per run instead. A `dl` that is installed and cannot be
used says so in the launch notice, because that is the fixable case — an absent
`dl` stays quiet, since a repo may carry a `devcontainer.json` for its editor
users on a machine that never wanted containers.

Installing `wf` from the channel brings a conforming `dl` with it: the recipe
declares `devlaunch >=0.0.24`, so the container half of the binary is not
invisibly absent on a fresh machine. That is what takes `wf` from ~3 MB to
~370 MB where devlaunch is not already present.

**`WF_PREWARM=1` needs `dl` 0.0.24 or newer** — the same floor, and the release
that set it. `dl <workspace> up` — start without attaching — and the
per-workspace launch lock are what the warm-up is made of.

**Two older floors, now subsumed by the one above.** Both are kept as the
record of why a floor exists at all — they are what an older `dl` did, and
0.0.24 is above both, so nothing `wf` will isolate with can still do either.

Launching several tickets at once means several `dl` processes preparing the
same repo's cache at the same moment; **0.0.20** is where those runs serialize
over a per-repo lock instead of racing (the loser of the old race could delete
the winner's half-written clone). And older `dl` on devpod 0.26 hands the agent
**no terminal**, and an agent with no terminal decides it was invoked
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
worked. `wf reap` removes the ones whose **work is over** — and the picker
[says so on its count line](#startup) without being asked, so running it is a
decision rather than a thing you have to remember:

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

**A `dl` that says nothing is not a `dl` saying nothing is at risk.** From
devlaunch **0.0.24** the listing answers `unsaved` for every clone `dl` made,
and leaves it null only on workspaces it did not make. So on 0.0.24 and newer a
missing answer about one of `wf`'s own workspaces means `dl`'s inspection fell
over, and the row is kept saying so rather than collected as clean. Releases
before that wrote null for a clean clone, and are still read that way. No single
row can tell the two apart, so `wf` asks `dl --version` once per run and reads
the listing accordingly; a `dl` it cannot place is read the older, permissive
way, so the worst case is the behaviour that shipped before the floor existed.

One `gh api graphql` call per repo answers all of this, however many workspaces
that repo has — which is also what makes the picker's background reading cheap
enough to take on every start.

**Deleting stays yours.** `wf` notices; it never reaps unattended. The plan is
printed so a reason you disagree with can be caught while "no" is still an
answer, and the count-line hint is the same reading with the same posture: it
names what would go and stops there.

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
