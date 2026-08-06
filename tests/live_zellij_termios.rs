//! Regression test for #30: a session `wf` creates must have **cooked** panes,
//! even though `wf` holds a raw-mode terminal the whole time it is up.
//!
//! The bug this guards is invisible from inside `wf` and silent at the boundary.
//! `wf` is a ratatui TUI, so its tty is raw. A zellij client reads the termios it
//! gives new panes **from its own stdin**, and the server then stamps that state
//! onto every pane it ever creates, for its whole lifetime. Because
//! `tokio::process::Command::output()` pipes only stdout and stderr — leaving
//! stdin *inherited*, unlike `std::process::Command::output()` — every session
//! `wf` created was born raw, and the agent in it ran on a tty with `-echo`
//! (keys invisible), `-isig` (`Ctrl-c` inert) and `-opost` (`\n` with no carriage
//! return, so output staircases). That is the "broken terminal after a
//! cross-session launch" of #30.
//!
//! So this test does the one thing the unit tests cannot: it makes **this
//! process's stdin a genuinely raw tty**, then goes through the real
//! `ensure_session` / `create_or_focus_tab` path and asks the pane itself what
//! termios it got. It lives in its own file because it mutates fd 0, which is
//! process-wide; libtest gives each integration test file its own binary.
//!
//! Needs `zellij` on PATH.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use wf::launch::{create_or_focus_tab, ensure_session, tab_label, TabOutcome};
use wf::model::{classify, Ticket, TicketType};

/// The flags that decide whether a terminal is usable at all. Each is asserted
/// present *and* not negated, because `stty -a` writes the off state as `-flag`.
const COOKED: [&str; 4] = ["echo", "icanon", "isig", "opost"];

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

/// Put a **raw** pty on this process's stdin, standing in for `wf`'s own
/// terminal while the picker is up.
///
/// The master fd is deliberately leaked: closing it would make every read on the
/// slave fail with `EIO`, and this process only ever needs the slave to *be* a
/// raw tty, never to carry traffic.
fn raw_tty_on_stdin() {
    unsafe {
        let mut master = 0;
        let mut slave = 0;
        assert_eq!(
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
            0,
            "openpty failed"
        );
        assert!(libc::dup2(slave, libc::STDIN_FILENO) >= 0, "dup2 failed");
        let mut settings: libc::termios = std::mem::zeroed();
        assert_eq!(
            libc::tcgetattr(libc::STDIN_FILENO, &mut settings),
            0,
            "tcgetattr failed"
        );
        libc::cfmakeraw(&mut settings);
        assert_eq!(
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &settings),
            0,
            "tcsetattr failed"
        );
        // Prove the premise rather than assume it: if stdin is not raw, a pass
        // below would mean nothing.
        let mut check: libc::termios = std::mem::zeroed();
        libc::tcgetattr(libc::STDIN_FILENO, &mut check);
        assert_eq!(
            check.c_lflag & libc::ECHO,
            0,
            "stdin should be raw (no echo)"
        );
    }
}

/// Split `stty -a` output into flag tokens (`echo`, `-icanon`, …), dropping the
/// `key = value` control-character assignments, which are not flags.
fn flags(stty: &str) -> Vec<&str> {
    stty.split([';', '\n'])
        .map(str::trim)
        .filter(|field| !field.contains('=') && !field.is_empty())
        .flat_map(str::split_whitespace)
        .collect()
}

#[tokio::test]
async fn a_session_wf_creates_has_cooked_panes() {
    raw_tty_on_stdin();

    let session = format!("wf-it-termios-{}", std::process::id());
    let guard = Throwaway(session.clone());
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = std::env::temp_dir().join(format!("{session}-stty.txt"));
    let _ = std::fs::remove_file(&report);

    // The stand-in agent: no real claude, it just reports the tty it was handed.
    // The `sleep` keeps the pane alive long enough to be observed as a live tab.
    let command: Vec<String> = [
        "bash".to_string(),
        "-lc".to_string(),
        format!("stty -a > {} 2>&1; sleep 5", report.display()),
    ]
    .to_vec();

    // The real path: `wf` creates the session while holding the raw tty above —
    // exactly what `Handoff::Quit` does, since a cross-session launch is
    // precisely the case where the target session must be created.
    ensure_session(&session, cwd)
        .await
        .expect("create the session");
    let ticket = Ticket {
        repo: "blooop/wayfinder".to_string(),
        number: 30,
        title: "cross-session handoff".to_string(),
        status: classify(true, false, vec![]),
        ticket_type: TicketType::Grilling,
    };
    let opened = create_or_focus_tab(&session, &tab_label(&ticket), cwd, &command)
        .await
        .expect("create the tab");
    assert_eq!(opened.outcome(), TabOutcome::Created);

    // The pane writes its termios once bash is up; poll rather than guess.
    let mut stty = String::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Ok(text) = std::fs::read_to_string(&report) {
            if text.contains("speed") {
                stty = text;
                break;
            }
        }
    }
    drop(guard);
    let _ = std::fs::remove_file(&report);
    assert!(!stty.is_empty(), "the pane never reported its termios");

    let flags = flags(&stty);
    for flag in COOKED {
        let off = format!("-{flag}");
        assert!(
            !flags.contains(&off.as_str()),
            "#30: pane was born with `{off}` — wf leaked its raw tty into the \
             zellij server again. Full stty:\n{stty}"
        );
        assert!(
            flags.contains(&flag),
            "expected `{flag}` in the pane's termios. Full stty:\n{stty}"
        );
    }
}
