//! The main screen: grouped flat list (frontier / claimed / blocked / done)
//! with the minimal row — state glyph, repo, number, title, `— needs #N` on
//! blocked rows — per the #8 and #9 resolutions. Read-only in Build 1.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::model::{Map, Status, GROUP_LABELS};

fn glyph_style(status: &Status) -> Style {
    match status {
        Status::Frontier => Style::new().fg(Color::Green),
        Status::Claimed => Style::new().fg(Color::Yellow),
        Status::Blocked { .. } => Style::new().fg(Color::Red),
        Status::Done => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The grouped list body as styled lines (header + rows per group, in fixed
/// group order). Shared by the live draw and the TestBackend tests.
pub fn body_lines(map: &Map) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    for (group, label) in GROUP_LABELS.iter().enumerate() {
        let members: Vec<_> = map
            .tickets
            .iter()
            .filter(|t| t.status.group() == group)
            .collect();
        lines.push(Line::styled(
            format!("  {label} — {}", members.len()),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        for ticket in members {
            let mut spans = vec![
                Span::raw("    "),
                Span::styled(ticket.status.glyph().to_string(), glyph_style(&ticket.status)),
                Span::raw(format!(
                    " {:<10} #{:<4} {}",
                    ticket.repo, ticket.number, ticket.title
                )),
            ];
            if let Status::Blocked { needs } = &ticket.status {
                let needs: Vec<String> = needs.iter().map(|n| format!("#{n}")).collect();
                spans.push(Span::styled(
                    format!("  — needs {}", needs.join(", ")),
                    Style::new().add_modifier(Modifier::DIM),
                ));
            }
            let style = match ticket.status {
                Status::Done => Style::new().add_modifier(Modifier::DIM),
                _ => Style::new(),
            };
            lines.push(Line::from(spans).style(style));
        }
        lines.push(Line::default());
    }
    lines.pop();
    lines
}

/// Draw the full screen: bordered frame with the repo in the title, grouped
/// list body, then the anchored bottom chrome — the reserved (empty) AFK
/// slot line, the ticket count line (with the subtle last-refreshed
/// indicator, empty before the first background poll), and the key hints.
pub fn draw(frame: &mut Frame, map: &Map, refresh_indicator: &str) {
    let block = Block::bordered().title(format!(" wf · {} ", map.repo));
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let [body_area, _afk_slot, count_area, hint_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1), // AFK agents slot — reserved, deliberately empty (see #7/#11)
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(body_lines(map)), body_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(format!("  {}/{}", map.tickets.len(), map.tickets.len())),
            Span::styled(
                format!("  {refresh_indicator}"),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ])),
        count_area,
    );
    frame.render_widget(
        Paragraph::new("  q/esc quit").style(Style::new().add_modifier(Modifier::DIM)),
        hint_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Map, Ticket};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn fixture_map() -> Map {
        let t = |number: u64, title: &str, open: bool, assigned: bool, needs: Vec<u64>| Ticket {
            repo: "wayfinder".to_string(),
            number,
            title: title.to_string(),
            status: classify(open, assigned, needs),
        };
        Map {
            repo: "blooop/wayfinder".to_string(),
            title: "Map: wf".to_string(),
            tickets: vec![
                t(2, "Choose the stack", false, true, vec![]),
                t(6, "Re-entry breadcrumbs", true, false, vec![]),
                t(7, "Supervising AFK agents", true, false, vec![6]),
                t(9, "Main screen design", true, true, vec![]),
                t(14, "Breadcrumb markers", true, false, vec![6, 9]),
            ],
        }
    }

    /// Render the fixture through TestBackend and return the screen as text.
    fn render(map: &Map) -> String {
        render_with_indicator(map, "")
    }

    fn render_with_indicator(map: &Map, indicator: &str) -> String {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, map, indicator)).expect("draw");
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn groups_render_in_fixed_order_with_counts() {
        let screen = render(&fixture_map());
        let frontier = screen.find("FRONTIER — ready to claim — 1").expect("frontier header");
        let claimed = screen.find("CLAIMED — 1").expect("claimed header");
        let blocked = screen.find("BLOCKED — 2").expect("blocked header");
        let done = screen.find("DONE — 1").expect("done header");
        assert!(frontier < claimed && claimed < blocked && blocked < done);
    }

    #[test]
    fn rows_carry_state_glyph_repo_number_title() {
        let screen = render(&fixture_map());
        assert!(screen.contains("○ wayfinder  #6    Re-entry breadcrumbs"));
        assert!(screen.contains("◐ wayfinder  #9    Main screen design"));
        assert!(screen.contains("⊘ wayfinder  #7    Supervising AFK agents"));
        assert!(screen.contains("● wayfinder  #2    Choose the stack"));
    }

    #[test]
    fn blocked_rows_show_needs_suffix() {
        let screen = render(&fixture_map());
        assert!(screen.contains("#7    Supervising AFK agents  — needs #6"));
        assert!(screen.contains("#14   Breadcrumb markers  — needs #6, #9"));
    }

    #[test]
    fn bottom_chrome_has_count_line_and_quit_hint() {
        let screen = render(&fixture_map());
        assert!(screen.contains("5/5"));
        assert!(screen.contains("q/esc quit"));
        assert!(screen.contains("wf · blooop/wayfinder"));
    }

    #[test]
    fn count_line_carries_the_refresh_indicator() {
        let screen = render_with_indicator(&fixture_map(), "· ↻ 3s ago");
        assert!(screen.contains("5/5  · ↻ 3s ago"));
    }
}
