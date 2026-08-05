//! Auto-start of AFK research tickets (Build 6, #19) — the invariant
//! "**every frontier `research` ticket has a tab**", reconciled after every
//! healthy poll.
//!
//! Deliberately *not* an event handler (#18): a launch that fires only on a
//! closure `wf` personally witnessed misses a frontier that already existed at
//! startup, one unblocked while the laptop was shut, and a `Failed` poll that
//! recovers across the transition. Reconciliation makes startup and steady state
//! one idempotent code path, and closure-unblocks-dependents falls out for free
//! — the next poll simply sees a bigger frontier.
//!
//! Three properties hold it together:
//!
//! * **Create-only.** [`reconcile`] returns launches and nothing else; it has no
//!   vocabulary for closing a tab. #5 keeps EXITED tabs as post-mortems and #7
//!   leaves pruning to the human, so reconciling downward would delete evidence.
//! * **Dedup on tab existence**, never on the claim. That buys retry semantics:
//!   a dead agent's EXITED tab still counts as existing, so it is not respawned
//!   — the corpse *is* the "don't retry" record, and the human retries by
//!   pruning the tab.
//! * **Only repos whose latest poll is healthy.** A `RefreshEvent::Failed` leaves
//!   stale data, and a stale frontier could launch a tab for a ticket already
//!   closed on another machine.
//!
//! [`reconcile`] is pure — a function of (frontier, tab names, poll health) — so
//! the whole decision is testable with no zellij and no network. The impure half
//! is only [`crate::launch::tabs_by_session`] before it and [`start`] after it.
//!
//! The event loop keeps just the part that needs the screen: gating the whole
//! thing on a poll report having landed (so the cadence is the poller's ~4s and
//! not the 250ms keyboard tick), and putting the resulting notice and agent-tab
//! count on the count line.
//!
//! No off switch, no stagger, no fan-out cap: quitting `wf` is the switch, and
//! the population is typically zero to two.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::launch::{
    execute, find_tab, plan, Handoff, Host, Launch, MapIssues, Mode, OpenTab, Targets,
};
use crate::model::{Status, Ticket};
use crate::projects::Checkout;
use crate::refresh::RefreshEvent;

/// What reconciliation knows about one repo's data freshness.
///
/// Three states, not a bool: "no poll has finished yet" is a different fact from
/// "the last poll failed", and collapsing them would either reconcile at startup
/// (which #18 rules out) or treat a first-tick repo as broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollHealth {
    /// No poll cycle has completed for this repo yet. Nothing is *known* fresh,
    /// so nothing is reconciled — this is what puts the first reconcile on the
    /// first poll tick rather than at startup.
    Awaiting,
    /// The latest poll succeeded: either a fresh map ([`RefreshEvent::Updated`])
    /// or a 304 confirming the current one ([`RefreshEvent::Unchanged`]).
    Fresh,
    /// The latest poll failed. The frontier in hand may be stale, so this repo
    /// is skipped until a poll succeeds again.
    Stale,
}

/// Latest poll health per repo slug.
///
/// A newtype rather than a bare map so that reading it is *total*: a repo the
/// poller has not reported on yet answers [`PollHealth::Awaiting`] instead of
/// making every caller decide what a missing key means.
#[derive(Debug, Clone, Default)]
pub struct PollHealthByRepo(BTreeMap<String, PollHealth>);

impl PollHealthByRepo {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Fold one poll outcome in. The only place a [`RefreshEvent`] becomes a
    /// health verdict, matched exhaustively so a new event variant has to state
    /// whether it means the frontier can be trusted.
    pub fn record(&mut self, repo: &str, event: &RefreshEvent) {
        let health = match event {
            RefreshEvent::Updated(_) | RefreshEvent::Unchanged => PollHealth::Fresh,
            RefreshEvent::Failed => PollHealth::Stale,
        };
        self.0.insert(repo.to_string(), health);
    }

    /// This repo's latest poll health — total, defaulting to
    /// [`PollHealth::Awaiting`].
    pub fn of(&self, repo: &str) -> PollHealth {
        self.0.get(repo).copied().unwrap_or(PollHealth::Awaiting)
    }
}

/// Tab names per zellij session, as last read.
///
/// Presence is load-bearing and the two cases are distinct: `Some(vec![])` is
/// "asked, and this session holds no tabs" (true of a session that does not
/// exist yet), while an **absent** session is "could not ask". Unknown tab state
/// must never be mistaken for an empty one, or a failed query would spawn a
/// duplicate agent on a ticket that already has a live tab.
pub type TabsBySession = BTreeMap<String, Vec<String>>;

/// Could this ticket need a tab at all? The half of eligibility that reads only
/// the ticket and its repo's poll health — type, status, freshness — with tab
/// existence and the projects cache still to come.
fn candidate(ticket: &Ticket, health: &PollHealthByRepo) -> bool {
    health.of(&ticket.repo) == PollHealth::Fresh
        && ticket.ticket_type.auto_startable()
        && matches!(ticket.status, Status::Frontier)
}

/// Is there anything worth reading the tab strip for?
///
/// [`reconcile`] needs tab names, and getting them costs a `zellij` subprocess
/// per session on every poll — so the driver asks this first and skips the round
/// trip on a quiet poll, which is nearly every poll. It shares
/// [`candidate`] with `reconcile` rather than restating the filter, so the
/// short-circuit cannot drift away from the decision it is short-circuiting.
pub fn any_candidate(tickets: &[Ticket], health: &PollHealthByRepo) -> bool {
    tickets.iter().any(|t| candidate(t, health))
}

/// Every launch needed to restore the invariant, given a frontier, the tabs
/// that exist, and each repo's poll health. Pure: no zellij, no network, no
/// clock.
///
/// A ticket earns a launch only if all of these hold:
///
/// 1. its repo's latest poll is [`PollHealth::Fresh`];
/// 2. it is `research` ([`crate::model::TicketType::auto_startable`]);
/// 3. it is on the **frontier** — open, unclaimed, unblocked. A claimed
///    research ticket is somebody's (possibly this very agent's, one poll
///    later), and `wf` does not pile on;
/// 4. its repo has a map issue to hand `/wayfinder`, and at least one
///    registered checkout on this machine;
/// 5. no candidate session already holds its tab, and the tab state of the
///    session that would host it is actually known.
///
/// Where a repo has several registered checkouts (the k1–k5 pattern) there is no
/// human to ask which one hosts the tab, so the **first** candidate in cache
/// order wins — deterministic, and near-immaterial for research, which reads the
/// repo and writes to the tracker rather than to the tree. Dedup still looks at
/// *every* candidate session, so a tab opened by hand in k2 stops a second one
/// appearing in k1.
pub fn reconcile(
    tickets: &[Ticket],
    checkouts: &[Checkout],
    map_issues: &MapIssues,
    tabs: &TabsBySession,
    health: &PollHealthByRepo,
) -> Vec<Launch> {
    tickets
        .iter()
        .filter(|t| candidate(t, health))
        .filter_map(|ticket| {
            let &map_issue = map_issues.get(&ticket.repo)?;
            let candidates = match plan(checkouts, ticket, map_issue, Mode::Afk) {
                Targets::Unregistered => return None,
                Targets::One(launch) => vec![launch],
                Targets::Many(launches) => launches,
            };
            // Create-only, deduped on tab existence: if any candidate session
            // already shows this ticket's tab — running, or an EXITED corpse —
            // there is nothing to do.
            let already_open = candidates.iter().any(|l| {
                tabs.get(&l.session)
                    .is_some_and(|names| find_tab(names, l.key()).is_some())
            });
            if already_open {
                return None;
            }
            let host = candidates.into_iter().next()?;
            // Unknown tab state in the very session that would host it: sit
            // this poll out rather than risk duplicating a live agent.
            if !tabs.contains_key(&host.session) {
                return None;
            }
            Some(host)
        })
        .collect()
}

/// Perform one auto-started launch, and return the tab it opened.
///
/// The AFK counterpart of the binary's `perform_launch`, and it lives here
/// rather than in the event loop because — unlike that one — it must never touch
/// the terminal: nobody asked for this tab, so suspending the TUI to attach
/// would yank the screen away from whatever the human was actually doing. Having
/// no terminal to own is also what makes the whole chain testable against a real
/// zellij (`tests/live_zellij_launch.rs`).
///
/// The mode is checked **before** [`crate::launch::execute`], not after. A HITL
/// launch's focus-moving steps run *inside* `execute`
/// ([`crate::launch::focus_steps`]), so by the time a [`Handoff`] came back the
/// human's zellij client would already have been moved. [`reconcile`] only ever
/// plans `Mode::Afk` and a test above pins that; this is the second lock, on the
/// side that would do the damage.
pub async fn start(launch: &Launch, host: &Host) -> Result<OpenTab> {
    match launch.mode {
        Mode::Afk => {}
        Mode::Hitl => bail!("auto-start refuses a HITL launch: {}", launch.describe()),
    }
    let (tab, handoff) = execute(launch, host).await?;
    match handoff {
        Handoff::Stay => Ok(tab),
        // Unreachable while `launch::handoff` maps every AFK launch to `Stay`.
        // Matched rather than ignored so that changing it there has to come back
        // through here and answer for it.
        Handoff::Suspend(_) => bail!("auto-start will not suspend the TUI"),
        Handoff::Quit => bail!("auto-start will not hand the terminal over"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Map, TicketType};
    use std::path::PathBuf;

    const WF: &str = "blooop/wayfinder";
    const ROS: &str = "kinisi/kinisi_ros";

    fn ticket(repo: &str, number: u64, ticket_type: TicketType, status: Status) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: format!("ticket {number}"),
            status,
            ticket_type,
        }
    }

    /// A frontier ticket of the given type — the shape auto-start acts on.
    fn frontier(repo: &str, number: u64, ticket_type: TicketType) -> Ticket {
        ticket(repo, number, ticket_type, classify(true, false, vec![]))
    }

    fn checkout(path: &str, repo: &str, session: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(path),
            repo: repo.to_string(),
            session: session.to_string(),
        }
    }

    /// One checkout of wayfinder, two of kinisi_ros (the k1–k5 pattern).
    fn cache() -> Vec<Checkout> {
        vec![
            checkout("/data/k1/kinisi_ros", ROS, "k1"),
            checkout("/data/k2/kinisi_ros", ROS, "k2"),
            checkout("/data/proj/wayfinder", WF, "wayfinder"),
        ]
    }

    fn map_issues() -> MapIssues {
        let mut m = MapIssues::new();
        m.insert(WF.to_string(), 1);
        m.insert(ROS.to_string(), 4);
        m
    }

    /// Every session queried, each holding the given tabs.
    fn tabs(entries: &[(&str, &[&str])]) -> TabsBySession {
        entries
            .iter()
            .map(|(session, names)| {
                (
                    session.to_string(),
                    names.iter().map(|n| n.to_string()).collect(),
                )
            })
            .collect()
    }

    /// All three sessions queried and empty — the ordinary "nothing running" read.
    fn no_tabs() -> TabsBySession {
        tabs(&[("k1", &[]), ("k2", &[]), ("wayfinder", &[])])
    }

    fn healthy() -> PollHealthByRepo {
        let mut h = PollHealthByRepo::new();
        h.record(WF, &RefreshEvent::Unchanged);
        h.record(ROS, &RefreshEvent::Unchanged);
        h
    }

    fn keys(launches: &[Launch]) -> Vec<String> {
        launches.iter().map(|l| l.key().to_string()).collect()
    }

    fn run(tickets: &[Ticket], tabs: &TabsBySession, health: &PollHealthByRepo) -> Vec<Launch> {
        reconcile(tickets, &cache(), &map_issues(), tabs, health)
    }

    /// A frontier of every type, plus a research ticket in each non-frontier
    /// status — the whole eligibility surface in one list.
    fn mixed_frontier() -> Vec<Ticket> {
        vec![
            frontier(WF, 3, TicketType::Research),
            frontier(WF, 19, TicketType::Task),
            frontier(WF, 18, TicketType::Grilling),
            frontier(WF, 9, TicketType::Prototype),
            frontier(WF, 21, TicketType::Untyped),
            ticket(WF, 30, TicketType::Research, classify(true, true, vec![])),
            ticket(
                WF,
                31,
                TicketType::Research,
                classify(true, false, vec![18]),
            ),
            ticket(WF, 32, TicketType::Research, classify(false, false, vec![])),
        ]
    }

    #[test]
    fn only_frontier_research_tickets_are_launched() {
        let launches = run(&mixed_frontier(), &no_tabs(), &healthy());
        assert_eq!(keys(&launches), vec!["wayfinder#3"]);
        let launch = &launches[0];
        // Through the unchanged Build 4 seam: Mode::Afk and plan().
        assert_eq!(launch.mode, Mode::Afk);
        assert_eq!(launch.session, "wayfinder");
        assert_eq!(launch.cwd, PathBuf::from("/data/proj/wayfinder"));
        assert_eq!(launch.map_issue, 1);
        assert_eq!(launch.ticket, 3);
        assert_eq!(
            launch.agent_argv(),
            vec![
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "-p".to_string(),
                "/wayfinder 1 3".to_string()
            ]
        );
    }

    #[test]
    fn the_diff_is_exactly_the_research_frontier_minus_the_tabs_that_exist() {
        let tickets = vec![
            frontier(WF, 3, TicketType::Research),
            frontier(WF, 5, TicketType::Research),
            frontier(ROS, 7, TicketType::Research),
        ];
        // Nothing running: all three.
        assert_eq!(
            keys(&run(&tickets, &no_tabs(), &healthy())),
            vec!["wayfinder#3", "wayfinder#5", "kinisi_ros#7"]
        );
        // #3 already has a tab (with a title and zellij's activity marker on
        // top, as a real strip shows it): only the other two.
        let running = tabs(&[
            ("k1", &[]),
            ("k2", &[]),
            ("wayfinder", &["Tab #1", "wayfinder#3 GitHub Issues a… ⏳"]),
        ]);
        assert_eq!(
            keys(&run(&tickets, &running, &healthy())),
            vec!["wayfinder#5", "kinisi_ros#7"]
        );
        // Everything covered: an empty launch set, not an empty-ish one.
        let all = tabs(&[
            ("k1", &["kinisi_ros#7 read the docs"]),
            ("k2", &[]),
            ("wayfinder", &["wayfinder#3", "wayfinder#5"]),
        ]);
        assert!(run(&tickets, &all, &healthy()).is_empty());
    }

    #[test]
    fn a_second_pass_over_the_tabs_it_just_made_launches_nothing() {
        // Idempotence, which is what "restarting wf does not double-spawn" is:
        // feed the first pass's own launches back as the tab strip they create.
        let tickets = vec![
            frontier(WF, 3, TicketType::Research),
            frontier(ROS, 7, TicketType::Research),
        ];
        let first = run(&tickets, &no_tabs(), &healthy());
        assert_eq!(first.len(), 2);
        let mut after = no_tabs();
        for launch in &first {
            after
                .get_mut(&launch.session)
                .expect("a queried session")
                .push(launch.label.to_string());
        }
        assert!(
            run(&tickets, &after, &healthy()).is_empty(),
            "a second reconcile over the same tabs must launch nothing"
        );
        // …and it stays empty across a retitle, because dedup is by key: the
        // tab wears the old label, the ticket generates a new one.
        let retitled: Vec<Ticket> = tickets
            .iter()
            .map(|t| Ticket {
                title: "renamed after the tab was made".to_string(),
                ..t.clone()
            })
            .collect();
        assert!(run(&retitled, &after, &healthy()).is_empty());
    }

    #[test]
    fn an_exited_corpse_still_counts_as_existing_so_a_dead_agent_is_not_retried() {
        // #5 keeps EXITED tabs deliberately, and reconciliation reads the tab
        // strip, not the claim — so the corpse is the "don't retry" record. To
        // this function a corpse is indistinguishable from a live tab, which is
        // the point.
        let tickets = vec![frontier(WF, 3, TicketType::Research)];
        let corpse = tabs(&[
            ("k1", &[]),
            ("k2", &[]),
            ("wayfinder", &["wayfinder#3 GitHub Issues a…"]),
        ]);
        assert!(run(&tickets, &corpse, &healthy()).is_empty());
    }

    #[test]
    fn a_repo_whose_latest_poll_failed_is_skipped_until_it_recovers() {
        let tickets = vec![
            frontier(WF, 3, TicketType::Research),
            frontier(ROS, 7, TicketType::Research),
        ];
        let mut health = healthy();
        health.record(WF, &RefreshEvent::Failed);
        // wayfinder's frontier may be stale (that ticket could have been closed
        // on another machine), so only the healthy repo reconciles.
        assert_eq!(
            keys(&run(&tickets, &no_tabs(), &health)),
            vec!["kinisi_ros#7"]
        );
        // A later success un-skips it — no separate recovery path.
        health.record(
            WF,
            &RefreshEvent::Updated(Map {
                repo: WF.to_string(),
                title: "Map: wf".to_string(),
                tickets: Vec::new(),
            }),
        );
        assert_eq!(
            keys(&run(&tickets, &no_tabs(), &health)),
            vec!["wayfinder#3", "kinisi_ros#7"]
        );
    }

    #[test]
    fn nothing_reconciles_before_the_first_poll_tick() {
        let tickets = vec![frontier(WF, 3, TicketType::Research)];
        // Startup: pollers have reported nothing yet.
        assert_eq!(PollHealthByRepo::new().of(WF), PollHealth::Awaiting);
        assert!(run(&tickets, &no_tabs(), &PollHealthByRepo::new()).is_empty());
        // Another repo's poll landing does not vouch for this one.
        let mut only_ros = PollHealthByRepo::new();
        only_ros.record(ROS, &RefreshEvent::Unchanged);
        assert!(run(&tickets, &no_tabs(), &only_ros).is_empty());
    }

    #[test]
    fn health_folds_every_refresh_event_and_the_latest_one_wins() {
        let mut health = PollHealthByRepo::new();
        health.record(WF, &RefreshEvent::Failed);
        assert_eq!(health.of(WF), PollHealth::Stale);
        health.record(WF, &RefreshEvent::Unchanged);
        assert_eq!(health.of(WF), PollHealth::Fresh);
        health.record(WF, &RefreshEvent::Failed);
        assert_eq!(health.of(WF), PollHealth::Stale);
        // Health is per repo, never aggregated.
        assert_eq!(health.of(ROS), PollHealth::Awaiting);
    }

    #[test]
    fn a_multi_checkout_repo_takes_the_first_candidate_but_dedups_across_all_of_them() {
        let tickets = vec![frontier(ROS, 7, TicketType::Research)];
        let launches = run(&tickets, &no_tabs(), &healthy());
        assert_eq!(
            launches.len(),
            1,
            "one tab per ticket, not one per checkout"
        );
        assert_eq!(launches[0].session, "k1", "cache order decides");
        // A tab in the *other* candidate session still stops a second spawn.
        let in_k2 = tabs(&[
            ("k1", &[]),
            ("k2", &["kinisi_ros#7 read it"]),
            ("wayfinder", &[]),
        ]);
        assert!(run(&tickets, &in_k2, &healthy()).is_empty());
    }

    #[test]
    fn an_unqueryable_session_is_not_an_empty_one() {
        let tickets = vec![frontier(WF, 3, TicketType::Research)];
        // `wayfinder` absent from the map: its tab state is unknown, so sit
        // this poll out rather than risk duplicating a live agent.
        let unknown = tabs(&[("k1", &[]), ("k2", &[])]);
        assert!(run(&tickets, &unknown, &healthy()).is_empty());
        // Present-but-empty is a fact, and it does launch.
        assert_eq!(
            keys(&run(&tickets, &no_tabs(), &healthy())),
            vec!["wayfinder#3"]
        );
    }

    #[test]
    fn a_ticket_with_no_map_or_no_checkout_here_is_skipped_not_launched() {
        // Research on a repo that has no registered checkout on this machine.
        let orphan = vec![frontier("other/repo", 2, TicketType::Research)];
        let mut health = healthy();
        health.record("other/repo", &RefreshEvent::Unchanged);
        assert!(reconcile(&orphan, &cache(), &map_issues(), &no_tabs(), &health).is_empty());
        // Checkout registered, but the repo has no map to hand /wayfinder.
        let tickets = vec![frontier(WF, 3, TicketType::Research)];
        assert!(reconcile(
            &tickets,
            &cache(),
            &MapIssues::new(),
            &no_tabs(),
            &healthy()
        )
        .is_empty());
    }

    #[test]
    fn the_short_circuit_never_hides_a_launch_reconcile_would_have_made() {
        // If `any_candidate` says no, skipping the zellij read is safe — the
        // property the driver relies on.
        let health = healthy();
        for tickets in [
            Vec::new(),
            mixed_frontier(),
            vec![frontier(WF, 3, TicketType::Research)],
            vec![ticket(
                WF,
                3,
                TicketType::Research,
                classify(true, true, vec![]),
            )],
            vec![frontier(WF, 19, TicketType::Task)],
        ] {
            if !any_candidate(&tickets, &health) {
                assert!(
                    run(&tickets, &no_tabs(), &health).is_empty(),
                    "short-circuited a poll that had work: {tickets:?}"
                );
            }
        }
        // The two directions that matter, spelled out.
        assert!(!any_candidate(&[], &health));
        assert!(!any_candidate(
            &[frontier(WF, 19, TicketType::Task)],
            &health
        ));
        assert!(any_candidate(&mixed_frontier(), &health));
        // Nothing polled yet, so nothing is worth asking zellij about.
        assert!(!any_candidate(&mixed_frontier(), &PollHealthByRepo::new()));
    }

    #[test]
    fn reconcile_never_asks_for_a_tab_to_close() {
        // Create-only by type: the return is `Vec<Launch>`, so there is no way
        // to express a closure. The behavioural half — tabs for tickets that
        // have left the frontier are left strictly alone.
        let tickets = vec![
            ticket(WF, 3, TicketType::Research, classify(false, false, vec![])),
            ticket(WF, 5, TicketType::Research, classify(true, true, vec![])),
        ];
        let strip = tabs(&[
            ("k1", &[]),
            ("k2", &[]),
            ("wayfinder", &["wayfinder#3 done", "wayfinder#5 claimed"]),
        ]);
        assert!(run(&tickets, &strip, &healthy()).is_empty());
        // The tab strip handed in is untouched — nothing here mutates it.
        assert_eq!(strip["wayfinder"].len(), 2);
    }

    #[test]
    fn every_launch_reconcile_plans_is_afk() {
        // The driver (`main::perform_autostart`) refuses a HITL launch, because
        // a HITL one would move the human's zellij client with no keystroke
        // behind it. This is the same invariant from the planning side: no
        // frontier, however mixed, can talk `reconcile` into a HITL launch.
        let tickets = vec![
            frontier(WF, 3, TicketType::Research),
            frontier(WF, 5, TicketType::Research),
            frontier(ROS, 7, TicketType::Research),
        ];
        let launches = run(&tickets, &no_tabs(), &healthy());
        assert_eq!(launches.len(), 3);
        assert!(launches.iter().all(|l| l.mode == Mode::Afk));
        // And headless with it: `claude -p`, never an interactive `claude`.
        assert!(launches
            .iter()
            .all(|l| l.agent_argv().contains(&"-p".to_string())));
    }

    /// [`start`]'s one hard rule: it may only ever perform an AFK launch. A HITL
    /// one would run [`crate::launch::focus_steps`] and move the human's zellij
    /// client with no keystroke behind it — so the refusal has to happen *before*
    /// `execute`, which is what this pins. Nothing is created: the guard returns
    /// before `ensure_session`, so the invented session name below is never
    /// spoken to zellij and this test needs no zellij to run.
    #[tokio::test]
    async fn start_refuses_a_hitl_launch_before_touching_zellij() {
        let checkouts = vec![checkout("/data/proj/wayfinder", WF, "wf-guard-test")];
        let ticket = frontier(WF, 3, TicketType::Research);
        // Through the real `plan`, since `Launch` has no other constructor.
        let hitl = match plan(&checkouts, &ticket, 1, Mode::Hitl) {
            Targets::One(launch) => launch,
            other => panic!("expected one target, got {other:?}"),
        };
        let err = start(&hitl, &Host::Outside)
            .await
            .expect_err("a HITL launch must be refused");
        assert!(err.to_string().contains("HITL"), "got {err}");
    }
}
