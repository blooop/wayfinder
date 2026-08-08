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

`skills/` holds the five prompts `wf` execs — `wayfinder`, `wayfinder-auto`,
`wayfinder-one`, `tdd`, `review`. They are not documentation *about* `wf`; they
are what it runs, named literally in `launch::route`, and the package installs
them beside the binary so the two cannot drift (`src/skills.rs` says why at
length).

Two consequences for anyone editing here:

- **Editing a skill is editing `wf`'s behaviour.** A change to
  `skills/wayfinder/SKILL.md` ships in the next release exactly as a change to
  `src/` does, and belongs in the same PR as whatever routing change motivated
  it. `skills/wayfinder-one/SKILL.md` and `wayfinder-auto` link
  `../wayfinder/GITHUB_TRACKER.md` and `../wayfinder/LIFECYCLE.md` by relative
  path, so the five move as a set and the layout under `skills/` is load-bearing.
- **Adding a `Route` means adding a skill.** `skills::BUNDLED` lists what ships
  and a unit test asserts every `Route::label()` is in it, so a route pointing
  at a prompt the package does not carry fails the build rather than an agent
  launch.

To work on a skill against a released `wf`, link the checkout instead of the
package — `WF_SKILLS_DIR=$PWD/skills wf skills install` — and the edit is live
in the next session.

## Checks

CI runs four things, in the order they are cheap to fix, with **both**
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
  cargo doc --no-deps --all-features --locked'
```

Run all four before pushing, `cargo fmt --all` first — a formatting diff fails
the build before any of the interesting checks get a chance to run.

The `tests/live_*.rs` files are excluded from that test command on purpose: they
talk to real GitHub and drive a real pty. Run them by name when the thing you
changed is what they cover.

## House style

The code explains *why*, not *what*, and the doc comments carry the design
decisions — including the ones that were tried and dropped. Match that: a new
type or screen rule wants the reasoning next to it, and a comment that only
restates the line below it is worse than none. Illegal states are kept
unrepresentable rather than checked for at the point of use; where a sum type
already says something, do not add a bool that can disagree with it.

Every lint switched off in `Cargo.toml`, `clippy.toml` or `rustfmt.toml` carries
a comment saying why. Adding an `allow` means adding that comment too.
