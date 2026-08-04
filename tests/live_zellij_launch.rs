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
//!
//! Needs `zellij` on PATH.

use std::path::Path;
use std::process::Command;

use wf::launch::{
    count_agent_tabs, create_or_focus_tab, ensure_session, find_tab, query_tab_names,
    session_state, tab_exists, tab_key, tab_label, SessionState, TabKey, TabOutcome,
};
use wf::model::{classify, Ticket, TicketType};

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
        repo: "blooop/wayfinder".to_string(),
        number,
        title: title.to_string(),
        status: classify(true, false, vec![]),
        ticket_type: TicketType::Task,
    }
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
    let session = format!("wf-it-{}", std::process::id());
    let guard = Throwaway(session.clone());
    let ticket16 = ticket(16, "Build 4 — launch seam");
    let label = tab_label(&ticket16);
    let key = tab_key(&ticket16);
    // The stand-in agent. Never a real claude session.
    let command: Vec<String> = ["bash", "-c", "sleep 30"]
        .iter()
        .map(|s| s.to_string())
        .collect();
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
