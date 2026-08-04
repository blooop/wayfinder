//! Background refresh: the live-poll loop behind the TUI (Build 5, #17).
//!
//! Strategy (per the #3 data-plane research): `gh api graphql` has no ETags,
//! so the hot loop is a two-tier hybrid —
//!
//! 1. Every [`POLL_INTERVAL`], a conditional REST probe of the map's
//!    `sub_issues` endpoint with `If-None-Match`. A 304 costs zero rate
//!    limit and means nothing changed; only a 200 triggers rerunning the
//!    full GraphQL map query (2 points).
//! 2. Every [`FULL_REFRESH_EVERY`]th cycle, an unconditional GraphQL fetch
//!    regardless of the probe — the research left unverified whether
//!    edge-only changes (dependency add/remove) flip the `sub_issues` ETag,
//!    so this bounds that staleness at ~30 s.
//!
//! Two live-verified `gh` quirks shape the prober: `gh api` exits nonzero on
//! a 304 (it is not a 2xx), so the status line is parsed instead of the exit
//! code; and the ETag hashes the response body, so the probe requests
//! `per_page=100` — a truncated page could miss changes to later children.
//!
//! Failures never surface as errors: every outcome is a [`RefreshEvent`],
//! and a failed poll just leaves the UI on stale data with an indicator.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::fetch;
use crate::model::Map;

/// How often the background loop probes for changes.
pub const POLL_INTERVAL: Duration = Duration::from_secs(4);

/// Every Nth cycle skips the probe and fetches unconditionally (the
/// edge-only-change safety net; see module docs).
pub const FULL_REFRESH_EVERY: u32 = 8;

/// One poll cycle's outcome, sent to the UI loop. Never an `Err`: refresh
/// failure is a displayable state, not a crash.
#[derive(Debug)]
pub enum RefreshEvent {
    /// The tracker changed; here is the freshly fetched map.
    Updated(Map),
    /// The probe confirmed nothing changed (HTTP 304) — data verified fresh.
    Unchanged,
    /// The poll failed (network, auth, parse); keep showing stale data.
    Failed,
}

/// What the conditional probe learned.
enum Probe {
    /// 304 — the stored ETag still matches.
    Unchanged,
    /// 200 — something changed; carry the new ETag for the next cycle.
    Changed { etag: Option<String> },
}

/// The background poller for one map. Owns the ETag across cycles.
pub struct Poller {
    owner: String,
    repo: String,
    number: u64,
    etag: Option<String>,
    cycle: u32,
}

impl Poller {
    pub fn new(owner: &str, repo: &str, number: u64) -> Self {
        Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
            etag: None,
            cycle: 0,
        }
    }

    /// Run one poll cycle: conditional probe, then a full GraphQL fetch only
    /// if needed. Infallible by design — errors become [`RefreshEvent::Failed`].
    pub async fn poll_once(&mut self) -> RefreshEvent {
        self.cycle = self.cycle.wrapping_add(1);
        let force_full = self.cycle.is_multiple_of(FULL_REFRESH_EVERY);

        if !force_full {
            match self.probe().await {
                Ok(Probe::Unchanged) => return RefreshEvent::Unchanged,
                Ok(Probe::Changed { etag }) => self.etag = etag,
                Err(_) => return RefreshEvent::Failed,
            }
        }

        match fetch::fetch_map(&self.owner, &self.repo, self.number).await {
            Ok(map) => RefreshEvent::Updated(map),
            Err(_) => RefreshEvent::Failed,
        }
    }

    /// Conditional REST probe of the map's `sub_issues` list. `-i` prints the
    /// status line and headers; the body is discarded (the GraphQL query is
    /// the single source of parsed truth).
    async fn probe(&self) -> Result<Probe> {
        let mut args = vec!["api".to_string(), "-i".to_string()];
        if let Some(etag) = &self.etag {
            args.push("-H".to_string());
            args.push(format!("If-None-Match: {etag}"));
        }
        args.push(format!(
            "repos/{}/{}/issues/{}/sub_issues?per_page=100",
            self.owner, self.repo, self.number
        ));

        let output = Command::new("gh")
            .args(&args)
            .output()
            .await
            .context("failed to run `gh` for the refresh probe")?;

        // `gh api` exits 1 on a 304 (non-2xx), so classify by status line.
        let head = String::from_utf8_lossy(&output.stdout);
        parse_probe(&head, output.status.success())
    }
}

/// Classify a `gh api -i` response: 304 → unchanged, 2xx → changed (with the
/// new ETag pulled from the headers). Anything else is a real failure.
fn parse_probe(response_head: &str, exit_ok: bool) -> Result<Probe> {
    let status_line = response_head.lines().next().unwrap_or_default();
    if status_line.contains(" 304") {
        return Ok(Probe::Unchanged);
    }
    if exit_ok {
        let etag = response_head
            .lines()
            .take_while(|l| !l.trim().is_empty()) // headers end at the blank line
            .find_map(|l| l.strip_prefix("Etag: ").or_else(|| l.strip_prefix("ETag: ")))
            .map(|v| v.trim().to_string());
        return Ok(Probe::Changed { etag });
    }
    bail!("probe failed: {status_line}");
}

/// Spawn the poll loop on the tokio runtime; the UI drains the returned
/// channel with `try_recv` between frames. The task ends when the receiver
/// is dropped (quit).
pub fn spawn(mut poller: Poller) -> mpsc::UnboundedReceiver<RefreshEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let event = poller.poll_once().await;
            if tx.send(event).is_err() {
                return; // UI is gone
            }
        }
    });
    rx
}

/// Where the cursor lands after a refresh swaps the ticket list.
///
/// Identity wins over position: if the previously selected ticket (by
/// `(repo, number)`) still exists anywhere in the new order, the cursor
/// follows it. Only if it vanished does the cursor fall back to the same
/// index, clamped to the new length. A refresh must never teleport the
/// selection just because rows moved between groups.
pub fn preserve_cursor(
    old_selected: Option<(&str, u64)>,
    old_index: usize,
    new_order: &[(&str, u64)],
) -> usize {
    if let Some(sel) = old_selected {
        if let Some(idx) = new_order.iter().position(|k| *k == sel) {
            return idx;
        }
    }
    old_index.min(new_order.len().saturating_sub(1))
}

/// What the count line's refresh indicator knows: when data was last
/// verified fresh (an update or a 304), and whether the latest poll failed.
///
/// Stale is not the absence of freshness — it is a positive fact (a poll
/// failed since the last success), so it is its own variant rather than a
/// bool riding alongside a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// No poll has completed yet (initial fetch only).
    Initial,
    /// The last poll succeeded `secs_ago` seconds ago.
    Fresh { secs_ago: u64 },
    /// Polls are failing; data was last verified `secs_ago` seconds ago
    /// (`None` if no poll ever succeeded).
    Stale { secs_ago: Option<u64> },
}

impl Freshness {
    /// The subtle indicator text for the count line. Empty before the first
    /// poll completes.
    pub fn indicator(&self) -> String {
        match self {
            Freshness::Initial => String::new(),
            Freshness::Fresh { secs_ago } if *secs_ago < 2 => "· ↻ just now".to_string(),
            Freshness::Fresh { secs_ago } => format!("· ↻ {}", ago(*secs_ago)),
            Freshness::Stale { secs_ago: Some(s) } => format!("· stale {}", ago(*s)),
            Freshness::Stale { secs_ago: None } => "· stale".to_string(),
        }
    }
}

fn ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: (&str, u64) = ("wayfinder", 6);
    const B: (&str, u64) = ("wayfinder", 7);
    const C: (&str, u64) = ("wayfinder", 14);
    const D: (&str, u64) = ("other", 6); // same number, different repo

    #[test]
    fn cursor_follows_ticket_identity_when_rows_reorder() {
        // B was selected at index 1; refresh moves it to the top.
        assert_eq!(preserve_cursor(Some(B), 1, &[B, A, C]), 0);
        // …or to the bottom.
        assert_eq!(preserve_cursor(Some(B), 1, &[A, C, B]), 2);
    }

    #[test]
    fn cursor_stays_put_when_nothing_moved() {
        assert_eq!(preserve_cursor(Some(B), 1, &[A, B, C]), 1);
    }

    #[test]
    fn identity_is_repo_and_number_not_number_alone() {
        // A ("wayfinder"#6) selected; new list has "other"#6 earlier — must
        // not match it.
        assert_eq!(preserve_cursor(Some(A), 2, &[D, B, A]), 2);
    }

    #[test]
    fn vanished_ticket_falls_back_to_same_index_clamped() {
        // C vanished; cursor was at 2, new list has 2 rows → clamp to 1.
        assert_eq!(preserve_cursor(Some(C), 2, &[A, B]), 1);
        // Cursor was at 0 and that ticket vanished → stay at 0.
        assert_eq!(preserve_cursor(Some(A), 0, &[B, C]), 0);
    }

    #[test]
    fn empty_new_list_pins_cursor_to_zero() {
        assert_eq!(preserve_cursor(Some(A), 2, &[]), 0);
    }

    #[test]
    fn no_prior_selection_clamps_index() {
        assert_eq!(preserve_cursor(None, 5, &[A, B]), 1);
        assert_eq!(preserve_cursor(None, 0, &[A, B]), 0);
    }

    #[test]
    fn indicator_renders_each_freshness_state() {
        assert_eq!(Freshness::Initial.indicator(), "");
        assert_eq!(Freshness::Fresh { secs_ago: 0 }.indicator(), "· ↻ just now");
        assert_eq!(Freshness::Fresh { secs_ago: 7 }.indicator(), "· ↻ 7s ago");
        assert_eq!(Freshness::Fresh { secs_ago: 130 }.indicator(), "· ↻ 2m ago");
        assert_eq!(
            Freshness::Stale { secs_ago: Some(42) }.indicator(),
            "· stale 42s ago"
        );
        assert_eq!(Freshness::Stale { secs_ago: None }.indicator(), "· stale");
    }

    #[test]
    fn probe_classifies_304_as_unchanged_despite_nonzero_exit() {
        // gh api exits 1 on a 304; the status line is the truth.
        let head = "HTTP/2.0 304 Not Modified\nAccess-Control-Allow-Origin: *\n";
        assert!(matches!(parse_probe(head, false), Ok(Probe::Unchanged)));
    }

    #[test]
    fn probe_extracts_etag_on_200() {
        let head = "HTTP/2.0 200 OK\nEtag: W/\"abc123\"\nVary: Accept\n\n[{}]";
        match parse_probe(head, true) {
            Ok(Probe::Changed { etag }) => assert_eq!(etag.as_deref(), Some("W/\"abc123\"")),
            other => panic!("expected Changed, got {:?}", other.map(|_| ()).err()),
        }
    }

    #[test]
    fn probe_real_failure_is_an_error() {
        let head = "HTTP/2.0 502 Bad Gateway\n";
        assert!(parse_probe(head, false).is_err());
    }
}
