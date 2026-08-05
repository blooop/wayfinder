//! Integration test for the Build 4 launch seam (#16) against a real zellij.
//!
//! Everything happens in a throwaway **detached** session named after this
//! process, killed and `delete-session`d by a drop guard even if an assertion
//! panics. No agent is ever launched: `bash -c 'sleep 30'` stands in for
//! `claude /wayfinder …`, exactly as the #5 prototype did.
//!
//! What it proves, end to end through the real code paths:
//! 1. `ensure_session` creates a session that does not exist (and reports it
//!    Live afterwards — verified by re-reading `list-sessions`, since
//!    `zellij action` exits 0 even on failure).
//! 2. `create_or_focus_tab` is **idempotent by name**: the first call creates
//!    the `<repo>#<n>` tab, the second focuses the existing one instead of
//!    spawning a duplicate.
//! 3. The tab is findable and countable as an agent tab.
//! 4. Auto-start (#19) reconciles a research frontier **once**: the second
//!    reconcile over the tab the first one produced launches nothing, and an
//!    EXITED corpse still counts as existing.
//!
//! Needs `zellij` on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use tokio::sync::Mutex;

use wf::autostart::{reconcile, PollHealthByRepo, TabsBySession};
use wf::launch::{
    count_agent_tabs, create_or_focus_tab, ensure_session, find_tab, query_tab_names,
    session_state, tab_exists, tab_key, tab_label, tabs_by_session, MapIssues, Mode, SessionState,
    TabKey, TabOutcome,
};
use wf::model::{classify, Ticket, TicketType};
use wf::projects::Checkout;
use wf::refresh::RefreshEvent;

const REPO: &str = "blooop/wayfinder";

/// One zellij conversation at a time.
///
/// Measured while writing the #19 test (zellij 0.44.3): two
/// `zellij attach --create-background` calls in flight at once **wedge each
/// other** — both hang indefinitely rather than failing — and libtest runs the
/// tests in this file on parallel threads by default. Each test still owns its
/// own throwaway session; this only stops their session *creations* overlapping.
static ZELLIJ: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Does this displayed tab name belong to this ticket? Duplicate tab names are
/// legal in zellij, so *counting* the ones with this key is how the test proves
/// the seam deduplicates.
fn is_ours(name: &str, key: &TabKey) -> bool {
    TabKey::parse(name).as_ref() == Some(key)
}

/// A ticket, so the test goes through the real `tab_key` / `tab_label` split
/// (#20) rather than hand-written tab names.
fn ticket(number: u64, title: &str) -> Ticket {
    Ticket {
        repo: REPO.to_string(),
        number,
        title: title.to_string(),
        status: classify(true, false, vec![]),
        ticket_type: TicketType::Task,
    }
}

/// A frontier `research` ticket — the one shape auto-start acts on.
fn research(number: u64, title: &str) -> Ticket {
    Ticket {
        ticket_type: TicketType::Research,
        ..ticket(number, title)
    }
}

/// The stand-in for `claude -p "/wayfinder …"`. **No agent is ever launched by
/// this test**: reconciliation decides the session, cwd and label, and only the
/// command is swapped for a sleeper.
fn stand_in(script: &str) -> Vec<String> {
    ["bash", "-c", script]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Kills and deletes the throwaway session on drop, panic or not.
struct Throwaway(String);

impl Throwaway {
    fn zellij(&self, args: &[&str]) {
        let _ = Command::new("zellij")
            .args(args)
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_SESSION_NAME")
            .env_remove("ZELLIJ_PANE_ID")
            .output();
    }
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        self.zellij(&["kill-session", &self.0]);
        // A killed session stays serialized and would be resurrected stale by
        // the next same-name create — the #5 findings' resurrection gotcha.
        self.zellij(&["delete-session", &self.0]);
    }
}

fn sessions() -> String {
    let output = Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("ZELLIJ_PANE_ID")
        .output()
        .expect("zellij must be on PATH");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn create_then_focus_by_name_is_idempotent() {
    let _serial = ZELLIJ.lock().await;
    let session = format!("wf-it-{}", std::process::id());
    let guard = Throwaway(session.clone());
    let ticket16 = ticket(16, "Build 4 — launch seam");
    let label = tab_label(&ticket16);
    let key = tab_key(&ticket16);
    // The stand-in agent. Never a real claude session.
    let command = stand_in("sleep 30");
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));

    // 1. The session does not exist yet; ensure_session creates it detached.
    assert_eq!(
        session_state(&sessions(), &session),
        SessionState::Missing,
        "the throwaway name must be unused"
    );
    ensure_session(&session, cwd)
        .await
        .expect("create the session");
    assert_eq!(
        session_state(&sessions(), &session),
        SessionState::Live,
        "session must be live after ensure_session"
    );
    // Idempotent: a live session is left alone.
    ensure_session(&session, cwd)
        .await
        .expect("second ensure_session");
    assert_eq!(session_state(&sessions(), &session), SessionState::Live);

    // 2. First launch creates the tab.
    let first = create_or_focus_tab(&session, &label, cwd, &command)
        .await
        .expect("first create_or_focus_tab");
    assert_eq!(first.outcome(), TabOutcome::Created);
    // The tab wears the readable label; the key is what found it.
    assert_eq!(first.name(), label.to_string());
    assert!(
        first.name().contains("Build 4"),
        "name was {}",
        first.name()
    );
    let names = query_tab_names(&session).await.expect("tab names");
    assert!(tab_exists(&names, &key), "tab names were {names:?}");
    assert_eq!(count_agent_tabs(&names), 1, "tab names were {names:?}");

    // 3. Second launch of the same ticket focuses, never duplicates —
    //    zellij happily allows duplicate tab names, so this is on us.
    let second = create_or_focus_tab(&session, &label, cwd, &command)
        .await
        .expect("second create_or_focus_tab");
    assert_eq!(second.outcome(), TabOutcome::Existed);
    let names = query_tab_names(&session).await.expect("tab names");
    assert_eq!(
        names.iter().filter(|n| is_ours(n, &key)).count(),
        1,
        "exactly one {key} tab expected, names were {names:?}"
    );
    assert_eq!(count_agent_tabs(&names), 1);

    // 2b. The same ticket **retitled**: a different label, the same key, so it
    //     still focuses the tab that is there instead of duplicating it — the
    //     #20 defect, proven against real zellij. The name handed back is the
    //     existing tab's, which is what `go-to-tab-name` answers to.
    let retitled = tab_label(&ticket(16, "Prove the seam again"));
    assert_ne!(retitled.to_string(), label.to_string());
    let third = create_or_focus_tab(&session, &retitled, cwd, &command)
        .await
        .expect("retitled create_or_focus_tab");
    assert_eq!(
        third.outcome(),
        TabOutcome::Existed,
        "a retitled ticket must find its own tab"
    );
    assert_eq!(
        third.name(),
        label.to_string(),
        "the tab keeps its old name"
    );
    let names = query_tab_names(&session).await.expect("tab names");
    assert_eq!(
        names.iter().filter(|n| is_ours(n, &key)).count(),
        1,
        "no duplicate after a retitle, names were {names:?}"
    );
    assert_eq!(find_tab(&names, &key), Some(label.to_string().as_str()));

    // A second ticket in the same project is a second tab, not a replacement.
    let ticket7 = ticket(7, "Supervising detached AFK agents");
    let other = create_or_focus_tab(&session, &tab_label(&ticket7), cwd, &command)
        .await
        .expect("second ticket");
    assert_eq!(other.outcome(), TabOutcome::Created);
    let names = query_tab_names(&session).await.expect("tab names");
    assert_eq!(count_agent_tabs(&names), 2, "tab names were {names:?}");
    assert!(tab_exists(&names, &key) && tab_exists(&names, &tab_key(&ticket7)));

    drop(guard);
    // kill-session reaps the tabs' commands (#5 findings §5b): no orphans.
    assert_eq!(
        session_state(&sessions(), &session),
        SessionState::Missing,
        "the throwaway session must be gone"
    );
}

/// Auto-start (#19) end to end against real zellij, with the agent command
/// replaced by a sleeper: reconcile → materialise the tab it asked for →
/// reconcile again and get nothing.
///
/// This is the half of #19's done-condition that can be *proven* rather than
/// reasoned: "restarting wf does not double-spawn". The state a restart sees is
/// exactly the state the second reconcile is given here — the same frontier and
/// a tab strip read back out of a live zellij — because reconciliation keeps no
/// memory of its own between polls.
#[tokio::test]
async fn autostart_reconciles_a_research_frontier_exactly_once() {
    let _serial = ZELLIJ.lock().await;
    let session = format!("wf-it-auto-{}", std::process::id());
    let guard = Throwaway(session.clone());
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let watched = vec![session.clone()];

    // The launch inputs: this checkout, hosted by the throwaway session.
    let checkouts = vec![Checkout {
        path: cwd.clone(),
        repo: REPO.to_string(),
        session: session.clone(),
    }];
    let mut map_issues = MapIssues::new();
    map_issues.insert(REPO.to_string(), 1);
    let tickets = vec![
        research(3, "GitHub Issues as the live data plane"),
        // Same repo, same frontier, but a build task: never auto-started.
        ticket(19, "Build 6 — auto-start AFK research tickets"),
    ];
    let mut healthy = PollHealthByRepo::new();
    healthy.record(REPO, &RefreshEvent::Unchanged);
    let go = |tabs: &TabsBySession, health: &PollHealthByRepo| {
        reconcile(&tickets, &checkouts, &map_issues, tabs, health)
    };

    // 1. The session does not exist yet, and `tabs_by_session` says so as a
    //    *fact* (queried, no tabs) rather than as a gap in what it knows.
    assert_eq!(session_state(&sessions(), &session), SessionState::Missing);
    let tabs = tabs_by_session(&watched).await;
    assert_eq!(
        tabs.get(&session).map(|n| n.len()),
        Some(0),
        "a session zellij does not have holds no tabs; tabs were {tabs:?}"
    );
    // A session nobody asked about is *not* the same thing, and reconciliation
    // refuses to spawn into it rather than risk duplicating a live agent.
    assert!(go(&TabsBySession::new(), &healthy).is_empty());
    // Before any poll lands, nothing reconciles even though the tab is missing.
    assert!(go(&tabs, &PollHealthByRepo::new()).is_empty());

    // 2. First healthy poll: exactly one launch — the research ticket, AFK.
    let launches = go(&tabs, &healthy);
    assert_eq!(launches.len(), 1, "only the research ticket: {launches:?}");
    let launch = &launches[0];
    assert_eq!(launch.mode, Mode::Afk);
    assert_eq!(launch.session, session);
    assert_eq!(launch.cwd, cwd);
    assert_eq!(launch.key().to_string(), "wayfinder#3");
    // What *would* have run, asserted instead of executed.
    assert_eq!(
        launch.agent_argv(),
        vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "-p".to_string(),
            "/wayfinder 1 3".to_string()
        ]
    );

    // 3. Materialise it through the real seam, sleeper in place of the agent.
    ensure_session(&launch.session, &launch.cwd)
        .await
        .expect("create the session");
    let opened = create_or_focus_tab(
        &launch.session,
        &launch.label,
        &launch.cwd,
        &stand_in("sleep 30"),
    )
    .await
    .expect("create the auto-started tab");
    assert_eq!(opened.outcome(), TabOutcome::Created);

    // 4. Second poll over the tab the first one produced: nothing to do. This
    //    is the restart case — same frontier, tab strip read fresh from zellij.
    let tabs = tabs_by_session(&watched).await;
    assert!(
        tab_exists(&tabs[&session], launch.key()),
        "tabs were {:?}",
        tabs[&session]
    );
    assert!(
        go(&tabs, &healthy).is_empty(),
        "a second reconcile double-spawned: {:?}",
        go(&tabs, &healthy)
    );
    // Retitling the issue changes the label but not the key, so it still finds
    // the tab it already has (#20) — no duplicate on a rename either.
    let retitled = vec![research(3, "renamed after the tab was made")];
    assert!(
        reconcile(&retitled, &checkouts, &map_issues, &tabs, &healthy).is_empty(),
        "a retitle must not spawn a second tab"
    );

    // 5. The corpse case: a tab whose command has exited is kept by zellij (#5
    //    — no `--close-on-exit`), so it still counts as existing and the dead
    //    agent is *not* retried. The corpse is the "don't retry" record.
    let dead = research(7, "Supervising detached AFK agents");
    let dead_launch = reconcile(
        std::slice::from_ref(&dead),
        &checkouts,
        &map_issues,
        &tabs,
        &healthy,
    )
    .into_iter()
    .next()
    .expect("an untabbed research ticket is launched");
    create_or_focus_tab(
        &dead_launch.session,
        &dead_launch.label,
        &dead_launch.cwd,
        &stand_in("exit 0"),
    )
    .await
    .expect("create the tab that immediately dies");
    // Let the command exit; the tab must survive it.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let tabs = tabs_by_session(&watched).await;
    assert!(
        tab_exists(&tabs[&session], dead_launch.key()),
        "an EXITED tab must still be listed; tabs were {:?}",
        tabs[&session]
    );
    assert!(
        reconcile(&[dead], &checkouts, &map_issues, &tabs, &healthy).is_empty(),
        "an EXITED corpse must not be respawned"
    );

    // 6. A repo whose latest poll failed is skipped, so a stale frontier cannot
    //    open a tab for a ticket already closed elsewhere.
    let mut stale = PollHealthByRepo::new();
    stale.record(REPO, &RefreshEvent::Failed);
    let fresh_ticket = vec![research(11, "V1 scope cut")];
    assert!(reconcile(&fresh_ticket, &checkouts, &map_issues, &tabs, &stale).is_empty());
    assert_eq!(
        reconcile(&fresh_ticket, &checkouts, &map_issues, &tabs, &healthy).len(),
        1,
        "…and it launches again once a poll succeeds"
    );

    // Nothing here ever closed a tab: both survive to be pruned by hand.
    assert_eq!(count_agent_tabs(&tabs[&session]), 2, "tabs were {tabs:?}");

    drop(guard);
    assert_eq!(
        session_state(&sessions(), &session),
        SessionState::Missing,
        "the throwaway session must be gone"
    );
}
