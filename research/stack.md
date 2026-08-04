# Implementation stack: language + TUI toolkit

Resolves [#2](https://github.com/blooop/wayfinder/issues/2). All claims cite primary sources
(official docs, repos, registries) checked 2026-08-04.

## Recommendation: Rust + ratatui

Rust + ratatui wins on the two criteria that actually differentiate the stacks — fuzzy-matcher
quality (nucleo is the only candidate *better* than fzf, not an imitation of it) and mature
tree rendering — while tying Go on the single-static-binary conda story and losing only on raw
developer velocity. Go + Bubble Tea is a close second; Python + Textual is the velocity winner
but fails the packaging criterion hardest.

Zellij itself being Rust adds mild ecosystem affinity, but interop is CLI (`zellij action ...`)
regardless of language. No clearly superior fourth option surfaced (Zig/libvaxis, Rust cursive,
etc. beat none of the three on the stated criteria).

## Concrete library list

| Library | Version (verified) | Maturity notes |
|---|---|---|
| [`ratatui`](https://crates.io/crates/ratatui) | 0.30.2 (2026-06-19) | 41.7M downloads, 87 releases, very active — [repo](https://github.com/ratatui/ratatui) |
| [`nucleo`](https://crates.io/crates/nucleo) / [`nucleo-matcher`](https://crates.io/crates/nucleo-matcher) | 0.5.0 (2024-04-02) / 0.3.1 (2024-02-20) | Helix editor's matcher; 1.1M / 3.2M downloads. Smith-Waterman with two matrices — "finds the optimal match more often" than fzf with fzf-compatible scoring; benchmarks ~7-8x faster than skim, far faster than fzf on 3M items. MPL-2.0. Slow-moving but battle-tested via Helix — [repo](https://github.com/helix-editor/nucleo) |
| [`nucleo-picker`](https://crates.io/crates/nucleo-picker) | 0.11.1 (2026-01-03) | Ready-made fzf-style picker TUI on nucleo; use directly or as reference |
| [`tui-tree-widget`](https://crates.io/crates/tui-tree-widget) | 0.24.0 (2026-01-09) | EdJoPaTo's tree widget for ratatui, 1.0M downloads, actively released |
| [`tui-input`](https://crates.io/crates/tui-input) | 0.15.3 (2026-04-18) | Headless input widget, 1.7M downloads |
| `tokio` (process, sync, time) + crossterm `event-stream` | — | Official async pattern: [ratatui.rs async tutorial](https://ratatui.rs/tutorials/counter-async-app/) + [async-template](https://github.com/ratatui/async-template) (action enum, event/render channels) |
| [`octocrab`](https://crates.io/crates/octocrab) (optional) | 0.54.1 (2026-07-24) | 16.3M downloads; native GitHub client bonus — shelling to `gh` stays primary |

Existence proof for the whole concept: [television](https://github.com/alexpasmantier/television),
a general-purpose fuzzy finder built on ratatui + this ecosystem; the
[awesome-ratatui](https://github.com/ratatui/awesome-ratatui) list has 40+ third-party widget crates.

Packaging: conda-forge has a canonical Rust binary recipe — `${{ compiler('rust') }}` +
`cargo auditable install --locked` + `cargo-bundle-licenses`
([conda-forge Rust example](https://conda-forge.org/docs/maintainer/example_recipes/rust/));
rattler-build resolves `compiler('rust')` to `rust_{target}` with cross-compilation handled
([rattler-build compilers doc](https://rattler-build.prefix.dev/latest/compilers/)). Output is
one static binary — ideal for the prefix.dev/blooop channel.

## Strongest argument against

**Velocity.** ratatui is immediate-mode and deliberately unopinionated — you own the event loop,
the action/message plumbing, focus management, and list-state bookkeeping that Bubble Tea's Elm
runtime and Textual's widget/worker system give you for free. For a tool that is mostly "poll
`gh`, spawn `zellij action`, redraw," Bubble Tea's `tea.Cmd`/`tea.Tick` model (now stable v2.0.8,
2026-07-03, per the [Go module proxy](https://proxy.golang.org/github.com/charmbracelet/bubbletea/v2/@latest))
plus the GitHub CLI team's official [`cli/go-gh`](https://github.com/cli/go-gh) library would
likely ship a working v1 in half the time — and fzf's actual algorithm is importable in Go
(`github.com/junegunn/fzf/src/algo`, MIT, v0.74.2 published 2026-08-01 —
[pkg.go.dev](https://pkg.go.dev/github.com/junegunn/fzf/src/algo)).

Secondary concern: nucleo's core last released April 2024 — stable and exercised daily by Helix,
but slow-moving.

**Fallback**: if a working tool this month matters more than the best matcher and tree story,
Go + Bubble Tea v2 + go-gh is the defensible second choice.

## Per-criterion comparison

| Criterion | Rust + ratatui | Go + Bubble Tea | Python + Textual |
|---|---|---|---|
| **1. Fuzzy picker** | **Best.** nucleo: fzf-compatible scoring, more optimal matches, faster ([repo](https://github.com/helix-editor/nucleo)); turnkey [`nucleo-picker`](https://crates.io/crates/nucleo-picker) | Good. Bubbles `list` has built-in fuzzy filtering via `sahilm/fuzzy` (Sublime-style, v0.1.3 pre-1.0 — [go.mod](https://github.com/charmbracelet/bubbles/blob/master/go.mod), [pkg.go.dev](https://pkg.go.dev/github.com/sahilm/fuzzy)); fzf's own `FuzzyMatchV2` importable, MIT ([pkg.go.dev](https://pkg.go.dev/github.com/junegunn/fzf/src/algo)) | OK. Built-in `textual.fuzzy` (`FuzzySearch`/`Matcher`, powers the command palette — [source](https://github.com/Textualize/textual/blob/main/src/textual/fuzzy.py)); simpler first-letter-bonus algorithm |
| **2. Tree/DAG (starmap)** | **Strong.** [`tui-tree-widget`](https://crates.io/crates/tui-tree-widget) 0.24.0 (Jan 2026, 1M dl); ratatui Canvas/braille for DAG edges | **Weakest.** No tree in Bubbles core ([README](https://github.com/charmbracelet/bubbles)); third-party [`tree-bubble`](https://github.com/savannahostrowski/tree-bubble) has 32 stars | **Best built-in.** Official `Tree` + `DirectoryTree` widgets, expand/collapse, cursor nav ([docs](https://textual.textualize.io/widgets/tree/)) |
| **3. Detached child processes, no pty** | `std::process` + [`tokio::process`](https://docs.rs/tokio/latest/tokio/process/) async spawn/wait; fits the action-channel pattern | [`os/exec`](https://pkg.go.dev/os/exec) + goroutines feeding `tea.Msg`s; `tea.ExecProcess` for foreground handoff ([repo](https://github.com/charmbracelet/bubbletea)) | [`asyncio` subprocess](https://docs.python.org/3/library/asyncio-subprocess.html) inside Textual async/thread workers ([workers guide](https://textual.textualize.io/guide/workers/)) |
| **4. `gh` interop** | Shell out fine; bonus: [octocrab](https://crates.io/crates/octocrab) 0.54.1 | **Best native.** [`cli/go-gh`](https://github.com/cli/go-gh): official GitHub CLI team lib — exec `gh`, REST+GraphQL clients, respects `GH_TOKEN`/`GH_HOST` | Shell out fine; PyGithub as bonus |
| **5. Conda on prefix.dev (rattler-build)** | **Tied best.** Single static binary; canonical `${{ compiler('rust') }}` recipe ([conda-forge](https://conda-forge.org/docs/maintainer/example_recipes/rust/), [rattler-build](https://rattler-build.prefix.dev/latest/compilers/)) | **Tied best.** Single binary; `${{ compiler('go-nocgo') }}` default, `go build -ldflags="-s -w"` ([conda-forge](https://conda-forge.org/docs/maintainer/example_recipes/go/)) | **Worst.** Drags interpreter + deps: rich, markdown-it-py, pygments, tree-sitter, … ([PyPI metadata](https://pypi.org/pypi/textual/json)) |
| **6. Async/polling refresh model** | Good but DIY: official tokio tutorial + async-template with event/action channels ([ratatui.rs](https://ratatui.rs/tutorials/counter-async-app/)) | **Best fit.** Elm architecture; `tea.Cmd`, `tea.Tick`, `tea.Every` are exactly this pattern ([repo](https://github.com/charmbracelet/bubbletea)) | Very good: `@work` async/thread workers, `exclusive=True`, auto-cleanup, `set_interval` ([workers guide](https://textual.textualize.io/guide/workers/)) |
| **Maturity / cadence** | ratatui 0.30.2 Jun 2026, biweekly-ish releases ([crates.io](https://crates.io/crates/ratatui)); nucleo core dormant since Apr 2024 | bubbletea v2.0.8 (2026-07-03), bubbles v2.1.1 (2026-07-04) — v2 stabilized recently after a long beta ([proxy](https://proxy.golang.org/github.com/charmbracelet/bubbletea/v2/@latest)); 44.1k stars | textual 8.2.8 (2026-06-30), steady 2-4 week cadence, 36.8k stars ([releases](https://github.com/Textualize/textual/releases)); Textualize the company shut down May 2025, project community-maintained by Will McGugan ([announcement](https://textual.textualize.io/blog/2025/05/07/the-future-of-textualize/)) |

## Verdict

**Rust + ratatui + nucleo + tui-tree-widget + tokio**, packaged with the conda-forge Rust recipe
pattern under rattler-build for the prefix.dev/blooop channel.
