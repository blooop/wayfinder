//! wf — the multi-project wayfinder ticket selector.
//!
//! Draw the list, pick a ticket, run the agent right there, exit. That is the
//! whole tool (#26): `wf` owns *selection only*, and the terminal you are
//! already in owns everything after it.
//!
//! * **Discovery** is accretive (#4/#15): running `wf` inside a checkout
//!   registers it in a per-machine cache — no registry, no background scan.
//! * **Data** is GitHub Issues via one `gh api graphql` query per map (#3),
//!   rendered as one cluster per open map (#50). The default screen is the
//!   leverage view (#51) — what can be taken now and what taking it unlocks —
//!   with the full blocking forest on `tab` and a nucleo query flattening
//!   either into one score-ordered list.
//! * **Startup** streams (#27): the screen is drawn before any network call,
//!   and the cached map numbers (#28) are already being fetched when it
//!   appears. Nothing polls afterwards — a warm start costs ~0.6 s, so
//!   re-running `wf` is cheaper than keeping it fresh.
//! * **Launch** replaces this process ([`launch::Launch::exec`]): `wf` gives
//!   the terminal back and becomes the selected agent with a `wf` skill
//!   invocation in the chosen checkout. Unattended work is not a feature — it
//!   is another terminal session you start and switch away from.

/// Write a whole report to stdout, tolerating a reader that has gone away.
///
/// `println!` panics on a closed pipe: Rust ignores `SIGPIPE`, so the write
/// returns `EPIPE` and the macro unwraps it. `wf skills | head` is an ordinary
/// thing to type and a panic is an absurd answer to it — the reader stopped
/// listening, which is not this program's problem to report. One write of one
/// string rather than a line at a time, so there is a single place for that to
/// be true.
///
/// Here rather than in the binary because [`reap::run`] emits from inside the
/// library and `wf skills` emits from outside it, and a second copy of this is
/// a second place for the `EPIPE` reasoning to be got wrong.
pub fn emit(text: &str) {
    use std::io::Write;
    let _ = std::io::stdout().write_all(text.as_bytes());
}

pub mod app;
pub mod fetch;
pub mod filter;
pub mod launch;
pub mod liveness;
pub mod model;
/// Test scaffolding shared by more than one module, and by both crates —
/// `src/probe.rs` is declared here *and* in `src/main.rs`, which is the only
/// way the binary's tests can reach it. Never compiled into a release.
#[cfg(test)]
mod probe;
pub mod projects;
pub mod reap;
pub mod reclaim;
pub mod refresh;
pub mod skills;
pub mod title;
pub mod ui;
pub mod view;
