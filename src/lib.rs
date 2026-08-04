//! wf — the multi-project wayfinder manager TUI.
//!
//! Build 3: accretive multi-project discovery (see #4/#15) — running `wf`
//! inside a checkout registers it in a per-machine cache; every cached
//! repo's `wayfinder:map` is found with one label search, fetched via
//! `gh api graphql`, merged into one grouped list behind a nucleo fuzzy
//! query (groups survive typing per #9), and kept fresh by one poller per
//! repo (#17). cwd-open focuses that project; `ctrl-g` widens.

pub mod app;
pub mod fetch;
pub mod filter;
pub mod model;
pub mod projects;
pub mod refresh;
pub mod ui;
