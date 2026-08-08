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
//!   the terminal back and becomes `claude "/wf <map> <n>"` in the
//!   chosen checkout. Unattended work is not a feature — it is another
//!   terminal session you start and switch away from.

pub mod app;
pub mod fetch;
pub mod filter;
pub mod launch;
pub mod model;
pub mod projects;
pub mod refresh;
pub mod skills;
pub mod ui;
pub mod view;
