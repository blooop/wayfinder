//! `wf` binary — fetch this repo's map live on startup, then run the main
//! screen: nucleo fuzzy query over the grouped list with the Build 2
//! keybinding skeleton (#14), kept fresh by the background refresh loop
//! (Build 5, #17). `ctrl-r` force-refreshes; `esc` clears the query then
//! quits; `q` quits on an empty query.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedReceiver;

use wf::app::{App, Outcome};
use wf::refresh::{Freshness, Poller, RefreshEvent};

// Build 1 hardcodes the one repo and its map issue; discovery is Build 3.
const OWNER: &str = "blooop";
const REPO: &str = "wayfinder";
const MAP_ISSUE: u64 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("wf: fetching {OWNER}/{REPO} map (#{MAP_ISSUE})…");
    let map = wf::fetch::fetch_map(OWNER, REPO, MAP_ISSUE).await?;
    let updates = wf::refresh::spawn(Poller::new(OWNER, REPO, MAP_ISSUE));

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, App::new(map), updates).await;
    ratatui::restore();
    result
}

/// Poll outcomes folded into displayable state: when data was last verified
/// fresh, and whether the latest poll failed.
struct RefreshState {
    last_verified: Option<Instant>,
    failing: bool,
}

impl RefreshState {
    fn apply(&mut self, event: &RefreshEvent) {
        match event {
            RefreshEvent::Updated(_) | RefreshEvent::Unchanged => {
                self.last_verified = Some(Instant::now());
                self.failing = false;
            }
            RefreshEvent::Failed => self.failing = true,
        }
    }

    fn freshness(&self) -> Freshness {
        let secs_ago = self.last_verified.map(|t| t.elapsed().as_secs());
        match (self.failing, secs_ago) {
            (true, secs_ago) => Freshness::Stale { secs_ago },
            (false, Some(secs_ago)) => Freshness::Fresh { secs_ago },
            (false, None) => Freshness::Initial,
        }
    }
}

async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut updates: UnboundedReceiver<RefreshEvent>,
) -> Result<()> {
    let mut refresh = RefreshState {
        last_verified: None,
        failing: false,
    };
    loop {
        // Drain every pending poll outcome before drawing. Data swaps in
        // place via App::replace_map, which keeps the cursor pinned to
        // ticket identity; query and scope are never reset.
        while let Ok(event) = updates.try_recv() {
            refresh.apply(&event);
            if let RefreshEvent::Updated(new_map) = event {
                app.replace_map(new_map);
            }
        }

        let indicator = refresh.freshness().indicator();
        terminal.draw(|frame| wf::ui::draw(frame, &app, &indicator))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.handle_key(key) {
                    Outcome::Quit => return Ok(()),
                    Outcome::Refresh => {
                        // Force refresh (ctrl-r): refetch in place, without
                        // waiting for the background poll cycle.
                        match wf::fetch::fetch_map(OWNER, REPO, MAP_ISSUE).await {
                            Ok(map) => {
                                app.replace_map(map);
                                refresh.apply(&RefreshEvent::Unchanged);
                                app.notice = Some("refreshed".to_string());
                            }
                            Err(err) => {
                                refresh.apply(&RefreshEvent::Failed);
                                app.notice = Some(format!("refresh failed: {err}"));
                            }
                        }
                    }
                    Outcome::Continue => {}
                }
            }
        }
    }
}
