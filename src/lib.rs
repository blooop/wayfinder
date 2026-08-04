//! wf — the multi-project wayfinder manager TUI.
//!
//! Build 1 (walking skeleton): fetch one hardcoded repo's map live via
//! `gh api graphql` and render the grouped list read-only.

pub mod fetch;
pub mod model;
pub mod refresh;
pub mod ui;
