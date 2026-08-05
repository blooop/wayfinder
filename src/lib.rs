//! wf — the multi-project wayfinder manager TUI.
//!
//! Build 3: accretive multi-project discovery (see #4/#15) — running `wf`
//! inside a checkout registers it in a per-machine cache; every cached
//! repo's `wayfinder:map` is found with one label search, fetched via
//! `gh api graphql`, merged into one grouped list behind a nucleo fuzzy
//! query (groups survive typing per #9), and kept fresh by one poller per
//! repo (#17). cwd-open focuses that project; `ctrl-g` widens.
//!
//! Build 4: the launch seam (see #16/#5/#7) — `enter` creates or focuses the
//! `<repo>#<n>` zellij tab in the project's session and hands the terminal
//! over to a HITL agent; `ctrl-a` spawns the same tab headless (AFK).
//!
//! Build 6: auto-start (see #19/#18) — after every healthy poll, `wf` reconciles
//! the invariant "every frontier `research` ticket has a tab" and spawns the
//! missing ones itself through that same AFK seam ([`autostart`]).

pub mod app;
pub mod autostart;
pub mod fetch;
pub mod filter;
pub mod launch;
pub mod model;
pub mod projects;
pub mod refresh;
pub mod ui;
