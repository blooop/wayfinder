# AGENTS.md

Notes for agents working in this repo. `README.md` is the real documentation —
what `wf` is, what every key does, and why the screen is shaped the way it is.
This file covers only what an agent needs that a user does not.

## Two binaries: `wf` and `wf-next`

`wf` on PATH is the **released** build, installed by pixi global from the
`blooop` channel (`~/.pixi/bin/wf`). Leave it alone: it is the thing that keeps
working while this checkout is mid-change, and it is what the user picks tickets
with.

`wf-next` is **this working tree**, built and copied to `~/.local/bin/wf-next`.
It is how a change in `src/` gets driven for real — the same binary, under a
name that cannot collide with the installed one, so both are on PATH at once and
running the wrong one is not possible by accident.

Refresh it after any change worth looking at:

```
cargo build --release --locked --bin wf && install -Dm755 target/release/wf ~/.local/bin/wf-next
```

A copy, deliberately, not a symlink into `target/`: `cargo clean` would leave a
dangling `wf-next` on PATH, and a snapshot that only moves when you ask it to is
easier to reason about than one that changes under you on the next build.

Two things to know about it:

- **It reports the crate version, not its provenance.** `wf-next --version` says
  `wf 0.5.1` — the same string a released 0.5.1 would say. The name is the only
  thing that distinguishes the builds, so if you need to know what is in a
  `wf-next`, the answer is "whatever the tree looked like the last time the
  command above was run", and the way to be sure is to run it again.
- **It fetches live GitHub data** through `gh`, the same as `wf`. It is a
  read-only picker until `enter`, which execs the agent on the ticket under the
  cursor — so drive it as far as the list and stop there unless launching is the
  thing being tested. `examples/preview_screen.rs` dumps the real screen against
  live maps non-interactively, with keys replayed from `$KEYS`, when you want to
  see a frame without taking over the terminal:

  ```
  KEYS="down right" cargo run --example preview_screen -- blooop/wayfinder 47
  ```

- **It cannot find its own skills.** `wf skills` resolves the bundle from the
  binary's own path — `<prefix>/share/wf/skills` for an installed package, or
  `<repo>/skills` for anything under `target/` — and a `wf-next` copied to
  `~/.local/bin` matches neither. Point it at the checkout explicitly:

  ```
  WF_SKILLS_DIR=$PWD/skills wf-next skills
  ```

  `target/release/wf` needs no such help: it is inside the repo, so it finds
  `skills/` on its own.

## The skills are part of this repo

`skills/` holds the six prompts `wf` execs — `wf`, `wf-mid`, `wf-auto`,
`wf-one`, `wf-tdd`, `wf-review`. They are not documentation *about* `wf`; they
are what it runs, named literally in `launch::route`, and the package installs
them beside the binary so the two cannot drift (`src/skills.rs` says why at
length).

Three consequences for anyone editing here:

- **Editing a skill is editing `wf`'s behaviour.** A change to
  `skills/wf/SKILL.md` ships in the next release exactly as a change to
  `src/` does, and belongs in the same PR as whatever routing change motivated
  it. `skills/wf-one/SKILL.md`, `wf-mid` and `wf-auto` link
  `../wf/GITHUB_TRACKER.md` and `../wf/LIFECYCLE.md` by relative
  path, so the six move as a set and the layout under `skills/` is load-bearing.
- **Adding a `Route` means saying which skill it invokes — or that it invokes
  none.** `skills::BUNDLED` lists what ships and `Route::bundled_skill()` is the
  exhaustive answer to which of them a route names; a unit test sweeps
  `Route::all()` and asserts every `Some` is in the bundle, so a route pointing
  at a prompt the package does not carry fails the build rather than an agent
  launch. `Route::Plain` (#112) is the one `None`: it execs the agent with no
  slash command, so there is no prompt for the package to be missing.
- **Renaming one leaves residue on every machine that had the old name.**
  `status` and `install` both iterate `BUNDLED`, so a link named for a skill
  that left the list is invisible to them while staying on disk, dangling.
  `skills::sweep` is what clears it, and a rename is not finished until the new
  names are in `BUNDLED` — that list is the only thing sweep can tell "ours, and
  no longer shipped" from "someone else's" by.

- **They reach the agent as a copy, not as a link into this tree.**
  `wf skills install` writes `~/.claude/wf-skills/<name>` and links
  `~/.claude/skills/<name> -> ../wf-skills/<name>`, because an isolated launch
  mounts `~/.claude` into the devcontainer and nothing else — a link into the
  pixi prefix dangles in there, and the symptom is `Unknown command: /wf-tdd`
  after the launch rather than an error anywhere near the install. The copy
  records the bundle it came from and every launch re-copies from it, so it
  cannot fall behind the tree you are editing.

To work on a skill against a released `wf`, install the checkout instead of the
package — `WF_SKILLS_DIR=$PWD/skills wf skills install` — and the edit is live
in the next session, however you launch.

## Checks

CI runs the checks below, in the order they are cheap to fix, with **both**
`RUSTFLAGS=-D warnings` and `RUSTDOCFLAGS=-D warnings` — so anything that warns
locally fails there. Set them when you run the checks, or the last one lies to
you: a doc comment linking to a private item is a warning locally and a failed
build in CI.

```
cargo fmt --all
RUSTFLAGS=-D\ warnings RUSTDOCFLAGS=-D\ warnings sh -c '
  cargo fmt --all --check &&
  cargo clippy --all-targets --all-features --locked &&
  cargo test --locked --lib --bins --examples &&
  cargo test --locked --test skill_docs &&
  cargo test --locked --test devcontainer_prebuild &&
  cargo test --locked --test toolchain_pin &&
  cargo test --locked --test offline_green &&
  cargo test --locked --test live_fetch -- common:: &&
  cargo doc --no-deps --all-features --locked'
```

Run the whole chain before pushing, `cargo fmt --all` first — a formatting diff
fails the build before any of the interesting checks get a chance to run.

The `tests/live_*.rs` files are excluded from that test command on purpose: they
talk to real GitHub, drive a real pty, or want a chosen `devlaunch`. Four
binaries under `tests/` are exceptions and run in the chain (and in CI) like any
unit test, because all four are offline checks on files-as-behavior:
`tests/skill_docs.rs` (shape checks on the skill docs' snippets),
`tests/devcontainer_prebuild.rs` (the contract between the devcontainer configs
and the workflow that publishes the prebuilt image to GHCR — see the comments in
`.devcontainer/devcontainer.json`), `tests/toolchain_pin.rs` (the guard that
keeps `rust-toolchain.toml` the only place a Rust version is written — see
**The Rust version is pinned in one place** below), and `tests/offline_green.rs`
(the guard on the exclusion itself: it reads `tests/live_*.rs` and fails if any
test in them is missing `#[ignore]`).

### The Rust version is pinned in one place

`rust-toolchain.toml` names the compiler, and nothing else in the repository
names one. Three of the four consumers get that for free, because rustup reads
the file for any cargo command inside the checkout: your shell, the
devcontainer, and the GitHub runners — which is why no workflow installs a
toolchain any more, and why `mcr.microsoft.com/devcontainers/rust:1-bookworm`'s
floating `1` tag no longer decides which compiler a container builds with. The
fourth is `recipe/recipe.yaml`, which resolves `rust` from conda-forge rather
than rustup and so reads the file explicitly with `load_from_file`; that is what
`--experimental` in `package.yml` is for.

**Bumping it is one edit to `channel`, and it waits on conda-forge.** The pin is
the newest version conda-forge packages, deliberately: the recipe compiles the
binary that ships, so a toolchain CI can install but the release build cannot
would mean testing with one compiler and shipping another. That gap is what let
clippy 1.98 reject `main` on a day nobody committed anything while the package
went on building fine (#173).

`tests/toolchain_pin.rs` is what stops a second home appearing. It fails on a
workflow that names a toolchain, on a recipe that goes back to a literal, and on
a `package.yml` that drops the flag the recipe's read needs.

### The live tests are gated, not absent

All 25 of them carry `#[ignore]` with a reason string saying what they need, so
a bare `cargo test` is green in a fresh checkout or a fresh devcontainer and
prints them as skipped — 25 of the 32 a full run reports ignored, the other
seven being the probe children in `src/` that only mean anything under recording
shims. Three consequences worth knowing before you touch any of this:

- **`cargo test -- --ignored` runs both halves of the gated set and only one of
  them can pass.** The eight that need a real GitHub (`live_fetch`,
  `live_discovery`, `live_streaming_startup`, `live_launch_exec`) want network,
  an authenticated `gh` and a checkout whose `origin` is `blooop/wayfinder`; the
  17 in `live_devlaunch` want a pixi environment and the `WF_CONTRACT_*` block,
  and panic rather than test whatever `dl` is on PATH. Run the first group by
  name — `cargo test --test live_fetch --test live_discovery -- --ignored` — and
  the second through `pixi run -e <env> contract`, which supplies the flag
  itself.
- **Each group has exactly one workflow.** `.github/workflows/live.yml` runs the
  gh-live four on push to `main` and on `workflow_dispatch`, not on pull
  requests, because two of them assert the tracker's present contents and a
  third asserts wall-clock budgets — a red run means the world moved.
  `.github/workflows/devlaunch-contract.yml` runs `live_devlaunch` in four
  pixi environments — three pinned devlaunch versions plus `default`, which
  pins none — and does block a pull request, because nothing in it depends on
  the tracker.
- **A new live test needs the attribute and a workflow line.** Without the
  `#[ignore]` it makes a bare `cargo test` fail on any machine lacking what it
  wants; with it and nothing else, it is a test that never runs anywhere.
  `live.yml` names its test binaries one by one for that reason — enrolment is
  a deliberate line, not a side effect. The attribute half is checked rather
  than remembered: `tests/offline_green.rs` walks `tests/live_*.rs` and fails
  on any test that is missing it. The workflow half is still on you.

## The devlaunch contract

`wf` shells out to `dl` four ways — `--version`, `--ls --json`, `<id> rm`, and
`<ws> up` / `<ws> -- <cmd>` — and every one of them is exercised in `src/` and
in `tests/live_launch_exec.rs` against a *recording shim*. That is the right
call for those tests (a machine with devlaunch installed and one without must
not take different paths through the same test) and it leaves the fixtures
unchecked against the program they describe. They have been wrong twice.

There is a fifth thing `wf` hands `dl`, and it is not an argument: the launch
`exec` sets two environment stamps of what `wf` itself did — the keystroke that
resolved to the exec, and when this node's prewarm fired, if one did (#160,
names and format minted by devlaunch#194). Two rules go with them. They are set
only where the exec *is* a `dl`, so a host launch carries neither and clears
both rather than leaving an inherited one in an agent's environment — as does
every other `dl` `wf` starts, which is what `launch::unstamped` is for; and neither
is ever a claim about how the launch went — a hit, a partial and a miss are
`dl`'s to observe from the arm it takes, and `wf` is gone by then. The seam is
observable only through a real exec, so its end-to-end guard is claim 6 of
`tests/live_launch_exec.rs`, and the variable names the README publishes are
compared to the ones the binary sets in `tests/skill_docs.rs`.

`pixi.toml` is here for that and nothing else: it installs a chosen `devlaunch`
and `tests/live_devlaunch.rs` asks it the questions `wf` asks it. **Pixi does
not build this crate** — the compiler is still rustup's, and adding `rust` to
those environments would give the repo two toolchains to keep in step for no
gain.

```
pixi run    suite             # the ordinary suite with no `dl` anywhere on PATH
pixi run -e default contract  # no devlaunch: the fallbacks
pixi run -e floor   contract  # devlaunch pinned to exactly launch::DEVLAUNCH_FLOOR
pixi run -e latest  contract  # whatever pixi.lock resolved
pixi run -e stale   contract  # 0.0.23 — below the floor, where wf must degrade
```

The contract test makes two kinds of claim. Most of it is about what `wf`
**reads** from a `dl` — the version, the listing, the `unsaved` field. Three
tests are about what it then **does**, and those are the ones that answer "is
the fallback real": `Isolation::detect` must return `Host` in a checkout that
really carries a devcontainer whenever `dl` is absent or below the floor, the
launch notice must say why, and `wf reap` with no `dl` must fail rather than
mistake "cannot see the workspaces" for "there are none". All three are checked
against a real absent-or-old devlaunch rather than a shim.

Three things follow for anyone editing here:

- **Raising `DEVLAUNCH_FLOOR` means editing `pixi.toml` in the same commit.**
  The `floor` environment pins the exact version that constant names, and
  `the_floor_environment_is_pinned_to_the_floor` compares them unconditionally —
  a floor nothing is ever run at is a floor nobody has checked. Note what it
  deliberately does *not* assert: that `DEVLAUNCH_FLOOR` equals
  `UNSAVED_IS_AN_OBJECT`. Those are two facts about two different questions and a
  floor bump has to be able to part them; a draft of this test tied them
  together, and the only way to satisfy that after a bump would have been to
  raise `UNSAVED_IS_AN_OBJECT` too, which walks `wf reap` straight back into
  devlaunch#171.
- **`pixi run suite` failing is not the contract breaking.** It is a test in
  `src/` that reached the ambient `dl`, which means it passes on your machine
  for a reason that has nothing to do with what it claims to check.
- **All four calls are run, two of them over a shimmed `devpod`.** `--version`
  and `--ls --json` go straight to the installed `dl`. `dl <id> rm` and
  `dl <ws> up` would build or destroy a container, so they run against a
  recording `devpod` on PATH — devlaunch's only devpod spawn is a bare name, so
  the real `dl` does its real argument parsing and workspace resolution and
  stops where the daemon would start. The verbs and flags come from
  `reap::removal_argv`, `launch::prewarm_argv` and `launch::isolated_argv`, so
  the test sends what the binary sends — with one limit worth knowing: the
  *workspace argument* is a devpod id, not the `owner/repo@wayfinder/<repo>-<n>`
  spec a real launch passes, because the spec form makes `dl` clone. A devlaunch
  change to how that spec is parsed or cloned is therefore not caught here. No
  daemon is needed and nothing creates a container.

- **Nothing whose output is captured inherits your environment.** The contract
  test records every argument `dl` hands devpod and prints that recording when
  an assertion fails, so a credential reaching argv would reach a CI log.
  `hermetic` is the only function in the file that starts a subprocess; it
  clears the environment and gives back three variables, one of which is
  devlaunch's own `DEVLAUNCH_NO_GH_TOKEN`.

  The qualifier is exact and was once missing. Two tests reach a real `dl`
  *without* going through `hermetic` — `Isolation::detect` and the launch-notice
  check both spawn `dl --version` from inside `src/launch.rs`, with the whole
  ambient environment — and they are meant to, because driving the production
  path is what they are for. `--version` consults no credential, prints none,
  and nothing captured from it is printed. Anything that *is* captured and
  printed goes through `hermetic`.

  Three guards keep that true rather than merely written down: one reads a real
  child's environment and compares it to the whole allowlist, one reads this
  file's own source and requires the single spawn to sit inside `hermetic`, and
  one refuses any capture holding a token-shaped string or a `NAME=value` whose
  name looks like a secret — matched on shape, so a variable neither repo has
  invented yet is still caught, and reported by name only.

`.github/workflows/devlaunch-contract.yml` runs all four environments on every
pull request, and once a week re-solves devlaunch first — that scheduled run is
the only thing that can discover a release published since the lock, and a red
one means the other repo moved rather than that this one is broken.

## House style

The code explains *why*, not *what*, and the doc comments carry the design
decisions — including the ones that were tried and dropped. Match that: a new
type or screen rule wants the reasoning next to it, and a comment that only
restates the line below it is worse than none. Illegal states are kept
unrepresentable rather than checked for at the point of use; where a sum type
already says something, do not add a bool that can disagree with it.

Every lint switched off in `Cargo.toml`, `clippy.toml` or `rustfmt.toml` carries
a comment saying why. Adding an `allow` means adding that comment too.
