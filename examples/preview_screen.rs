//! Dump the real main screen against live maps, non-interactively — a way to
//! see what `wf` will draw, and to drive its keys, without taking over the
//! terminal. Throwaway.
//!
//! Run: `cargo run --example preview_screen -- blooop/wayfinder 47 [more...]`
//! Keys to replay come from $KEYS, e.g. KEYS="down right right" — one of
//! down/up/left/right/tab/<char>.

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use wf::app::App;
use wf::model::MapId;
use wf::ui::body_lines;

fn press(app: &mut App, name: &str) {
    // "ctrl-j" sends the chord; a bare name sends it unmodified.
    let (mods, name) = match name.strip_prefix("ctrl-") {
        Some(rest) => (KeyModifiers::CONTROL, rest),
        None => (KeyModifiers::NONE, name),
    };
    let code = match name {
        "down" => KeyCode::Down,
        "up" => KeyCode::Up,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "tab" => KeyCode::Tab,
        // Named so the launch picker (#62/#96) is reachable from `$KEYS` —
        // without them `enter` would send a bare `e` into the query.
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        other => KeyCode::Char(other.chars().next().expect("a key")),
    };
    app.handle_key(KeyEvent::new(code, mods));
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ids: Vec<MapId> = args
        .chunks(2)
        .filter_map(|pair| match pair {
            [repo, number] => Some(MapId::new(repo, number.parse().expect("map number"))),
            _ => None,
        })
        .collect();

    let mut clusters = BTreeMap::new();
    for id in &ids {
        match wf::fetch::fetch_map(id).await {
            Ok(map) => {
                clusters.insert(id.clone(), map);
            }
            Err(e) => eprintln!("{}#{} failed: {e}", id.repo, id.number),
        }
    }

    let keys: Vec<String> = std::env::var("KEYS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let mut app = App::new(clusters);
    for key in &keys {
        press(&mut app, key);
    }
    if !keys.is_empty() {
        println!("── after: {} ──", keys.join(" "));
    }
    let stops = app.stops();
    let pos = app.cursor_pos();
    let label = |at: &wf::view::StopAt| match &at.stop {
        wf::view::Stop::Map(id) => format!("map #{}", id.number),
        wf::view::Stop::Ticket(row) => format!("#{}", app.ticket(row).number),
        wf::view::Stop::Group(g) => format!("{:?}", g.kind),
        wf::view::Stop::Project(repo) => format!("project {repo}"),
    };
    println!(
        "cursor: {} of {} → {} at depth {}",
        pos,
        stops.len(),
        stops.get(pos).map_or("nothing".into(), &label),
        stops.get(pos).map_or(-1, |at| at.depth as isize),
    );
    if std::env::var("NAV").is_ok() {
        for (i, at) in stops.iter().enumerate() {
            println!(
                "  {}{i:>3} depth {} {}",
                if i == pos { "▶" } else { " " },
                at.depth,
                label(at)
            );
        }
        return;
    }
    // Re-emit the real styles as ANSI so the orange marker is visible in a pipe.
    for line in body_lines(&app) {
        let mut out = String::new();
        for span in &line.spans {
            let marker = span.style.fg == Some(ratatui::style::Color::Indexed(208));
            let dim = span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM);
            let code = if marker {
                "\x1b[38;5;208;1m"
            } else if dim {
                "\x1b[2m"
            } else {
                "\x1b[0m"
            };
            out.push_str(code);
            out.push_str(&span.content);
        }
        println!("{out}\x1b[0m");
    }
}
