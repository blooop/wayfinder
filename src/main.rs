//! `wf` binary — fetch this repo's map live on startup, show the grouped
//! list, and keep it fresh via the background refresh loop (Build 5, #17).
//! `q`/`esc` quits.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedReceiver;

use wf::model::Map;
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
    let result = run(&mut terminal, map, updates);
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

fn run(
    terminal: &mut DefaultTerminal,
    mut map: Map,
    mut updates: UnboundedReceiver<RefreshEvent>,
) -> Result<()> {
    let mut refresh = RefreshState {
        last_verified: None,
        failing: false,
    };
    loop {
        // Drain every pending poll outcome before drawing. Data swaps in
        // place; input state (and, once #14 lands, cursor/query — merged via
        // refresh::preserve_cursor by ticket identity) is never reset.
        while let Ok(event) = updates.try_recv() {
            refresh.apply(&event);
            if let RefreshEvent::Updated(new_map) = event {
                map = new_map;
            }
        }

        let indicator = refresh.freshness().indicator();
        terminal.draw(|frame| wf::ui::draw(frame, &map, &indicator))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    _ => {}
                }
            }
        }
    }
}
