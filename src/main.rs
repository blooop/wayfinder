//! `wf` binary — Build 1 walking skeleton: fetch this repo's map live on
//! startup and show the grouped list read-only. `q`/`esc` quits.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use wf::model::Map;

// Build 1 hardcodes the one repo and its map issue; discovery is Build 3.
const OWNER: &str = "blooop";
const REPO: &str = "wayfinder";
const MAP_ISSUE: u64 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("wf: fetching {OWNER}/{REPO} map (#{MAP_ISSUE})…");
    let map = wf::fetch::fetch_map(OWNER, REPO, MAP_ISSUE).await?;

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &map);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, map: &Map) -> Result<()> {
    loop {
        terminal.draw(|frame| wf::ui::draw(frame, map))?;
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
