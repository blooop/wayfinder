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
    count_agent_tabs, create_or_focus_tab, ensure_session, query_tab_names, session_state,
    tab_exists, SessionState, TabOutcome,
};

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
    let tab = "wayfinder#16";
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
    let first = create_or_focus_tab(&session, tab, cwd, &command)
        .await
        .expect("first create_or_focus_tab");
    assert_eq!(first, TabOutcome::Created);
    let names = query_tab_names(&session).await.expect("tab names");
    assert!(tab_exists(&names, tab), "tab names were {names:?}");
    assert_eq!(count_agent_tabs(&names), 1, "tab names were {names:?}");

    // 3. Second launch of the same ticket focuses, never duplicates —
    //    zellij happily allows duplicate tab names, so this is on us.
    let second = create_or_focus_tab(&session, tab, cwd, &command)
        .await
        .expect("second create_or_focus_tab");
    assert_eq!(second, TabOutcome::Existed);
    let names = query_tab_names(&session).await.expect("tab names");
    assert_eq!(
        names.iter().filter(|n| n.trim() == tab).count(),
        1,
        "exactly one {tab} tab expected, names were {names:?}"
    );
    assert_eq!(count_agent_tabs(&names), 1);

    // A second ticket in the same project is a second tab, not a replacement.
    let other = create_or_focus_tab(&session, "wayfinder#7", cwd, &command)
        .await
        .expect("second ticket");
    assert_eq!(other, TabOutcome::Created);
    let names = query_tab_names(&session).await.expect("tab names");
    assert_eq!(count_agent_tabs(&names), 2, "tab names were {names:?}");
    assert!(tab_exists(&names, tab) && tab_exists(&names, "wayfinder#7"));

    drop(guard);
    // kill-session reaps the tabs' commands (#5 findings §5b): no orphans.
    assert_eq!(
        session_state(&sessions(), &session),
        SessionState::Missing,
        "the throwaway session must be gone"
    );
}
