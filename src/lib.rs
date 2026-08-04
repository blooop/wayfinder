//! wf — the multi-project wayfinder manager TUI.
//!
//! Build 2: fetch one hardcoded repo's map live via `gh api graphql` and
//! render the grouped list behind a nucleo fuzzy query with the keybinding
//! skeleton (see #14); groups survive typing per the #9 resolution.

pub mod app;
pub mod fetch;
pub mod filter;
pub mod model;
pub mod refresh;
pub mod ui;
