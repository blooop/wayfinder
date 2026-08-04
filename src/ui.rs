//! The main screen: grouped flat list (frontier / claimed / blocked / done)
//! with the minimal row — state glyph, repo, number, title, `— needs #N` on
//! blocked rows — per the #8 and #9 resolutions. Build 2 adds the fuzzy
//! query (2a: groups survive typing), the cursor, and the bottom prompt.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Overlay, Scope};
use crate::model::{Status, GROUP_LABELS};

fn glyph_style(status: &Status) -> Style {
    match status {
        Status::Frontier => Style::new().fg(Color::Green),
        Status::Claimed => Style::new().fg(Color::Yellow),
        Status::Blocked { .. } => Style::new().fg(Color::Red),
        Status::Done => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The grouped list body as styled lines (header + surviving rows per group,
/// in fixed group order). Shared by the live draw and the TestBackend tests.
///
/// Filtering is 2a per the #9 resolution: non-matching rows drop, every
/// group header stays (showing `matched/total` while a query is live), and
/// group order never changes. Focus mode drops the repo column — the
/// header already names the project.
pub fn body_lines(app: &App) -> Vec<Line<'static>> {
    let visible = app.visible();
    let scoped = app.scoped();
    let cursor_idx = visible.get(app.cursor_pos()).copied();
    let show_repo = app.scope == Scope::All;
    let filtering = !app.query.is_empty();

    let mut lines = vec![Line::default()];
    for (group, label) in GROUP_LABELS.iter().enumerate() {
        let total = scoped
            .iter()
            .filter(|&&i| app.map.tickets[i].status.group() == group)
            .count();
        let members: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| app.map.tickets[i].status.group() == group)
            .collect();
        let header = if filtering {
            format!("  {label} — {}/{}", members.len(), total)
        } else {
            format!("  {label} — {total}")
        };
        lines.push(Line::styled(
            header,
            Style::new().add_modifier(Modifier::BOLD),
        ));
        for idx in members {
            let ticket = &app.map.tickets[idx];
            let cursor = if cursor_idx == Some(idx) { '▶' } else { ' ' };
            let mut spans = vec![
                Span::raw(format!("  {cursor} ")),
                Span::styled(ticket.status.glyph().to_string(), glyph_style(&ticket.status)),
            ];
            if show_repo {
                spans.push(Span::raw(format!(
                    " {:<10} #{:<4} {}",
                    ticket.short_repo(),
                    ticket.number,
                    ticket.title
                )));
            } else {
                spans.push(Span::raw(format!(" #{:<4} {}", ticket.number, ticket.title)));
            }
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

/// The keybinding skeleton (#14): `tab` peek is deferred from v1; `enter`
/// enters the ticket's agent session and `ctrl-a` spawns it AFK (#16); `esc`
/// clears the query first and quits on an empty one, and `q` only quits when
/// the query is empty (mid-query it types).
const KEY_HINTS: &str =
    "  enter launch · ctrl-a afk · ctrl-f focus · ctrl-g all · ctrl-r refresh · esc quit";

/// The reserved AFK line (#1/#11): a count of the `<repo>#<n>` agent tabs
/// zellij is holding, as of the last check. Empty when there are none —
/// including before anything has been launched.
pub fn afk_line(app: &App) -> String {
    match app.agent_tabs {
        0 => String::new(),
        1 => "  agents: 1 tab".to_string(),
        n => format!("  agents: {n} tabs"),
    }
}

/// A centered box `width`×`height` (clamped) inside `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// The which-checkout modal: one row per registered checkout of the repo
/// (session name + path), because several checkouts of one repo each have
/// their own session and the human must say which hosts the tab.
fn draw_overlay(frame: &mut Frame, app: &App) {
    let Overlay::PickCheckout { launches, cursor } = &app.overlay else {
        return;
    };
    let mut lines = vec![Line::default()];
    for (i, launch) in launches.iter().enumerate() {
        let marker = if i == *cursor { '▶' } else { ' ' };
        lines.push(Line::from(vec![
            Span::raw(format!("  {marker} ")),
            Span::styled(
                format!("{:<12}", launch.session),
                Style::new().fg(Color::Cyan),
            ),
            Span::raw(format!(" {}", launch.cwd.display())),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  enter launch here · ↑/↓ pick · esc cancel",
        Style::new().add_modifier(Modifier::DIM),
    ));
    // The key, not the label: this asks *which checkout*, so the ticket only
    // needs identifying, and the title is already on the row behind the prompt.
    let tab = launches
        .first()
        .map(|l| l.key().to_string())
        .unwrap_or_default();
    let width = lines
        .iter()
        .map(|l| l.width() as u16 + 4)
        .chain(std::iter::once(tab.len() as u16 + 32))
        .max()
        .unwrap_or(40);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(format!(" which checkout hosts {tab}? "))
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

/// Draw the full screen: bordered frame with the scope in the title, grouped
/// list body, then the anchored bottom chrome — the AFK slot line (a count of
/// live agent tabs, empty when there are none), the match-count line (with
/// the subtle last-refreshed indicator from the background poll, empty before
/// the first cycle, plus any one-shot notice), the fzf-style prompt, and the
/// key hints. The which-checkout picker, when open, floats over all of it.
pub fn draw(frame: &mut Frame, app: &App, refresh_indicator: &str) {
    let mut block = match &app.scope {
        Scope::All => Block::bordered().title(format!(" wf · {} ", app.map.repo)),
        // The focused title names the project by its full slug — with one
        // project on screen there is room, and it disambiguates forks.
        Scope::Project(repo) => Block::bordered().title(format!(" wf · {repo} — focused ")),
    };
    if app.scope != Scope::All {
        block = block.title_top(Line::from(" ctrl-g all projects ").right_aligned());
    }
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let [body_area, afk_area, count_area, prompt_area, hint_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1), // AFK agents slot (see #7/#11)
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Paragraph::new(body_lines(app)), body_area);
    frame.render_widget(
        Paragraph::new(afk_line(app)).style(Style::new().fg(Color::Magenta)),
        afk_area,
    );

    let mut count_spans = vec![
        Span::raw(format!("  {}/{}", app.visible().len(), app.scoped().len())),
        Span::styled(
            format!("  {refresh_indicator}"),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ];
    if let Some(notice) = &app.notice {
        count_spans.push(Span::styled(
            format!("   {notice}"),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(count_spans)), count_area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  > ", Style::new().fg(Color::Cyan)),
            Span::raw(app.query.clone()),
            Span::styled("█", Style::new().add_modifier(Modifier::DIM)),
        ])),
        prompt_area,
    );
    frame.render_widget(
        Paragraph::new(KEY_HINTS).style(Style::new().add_modifier(Modifier::DIM)),
        hint_area,
    );
    draw_overlay(frame, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Map, Ticket, TicketType};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn fixture_map() -> Map {
        let t = |number: u64, title: &str, open: bool, assigned: bool, needs: Vec<u64>| Ticket {
            repo: "blooop/wayfinder".to_string(),
            number,
            title: title.to_string(),
            status: classify(open, assigned, needs),
            ticket_type: TicketType::Task,
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

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Render the app through TestBackend and return the screen as text.
    fn render(app: &App) -> String {
        render_with_indicator(app, "")
    }

    fn render_with_indicator(app: &App, indicator: &str) -> String {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, app, indicator)).expect("draw");
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
        let screen = render(&App::new(fixture_map()));
        let frontier = screen.find("FRONTIER — ready to claim — 1").expect("frontier header");
        let claimed = screen.find("CLAIMED — 1").expect("claimed header");
        let blocked = screen.find("BLOCKED — 2").expect("blocked header");
        let done = screen.find("DONE — 1").expect("done header");
        assert!(frontier < claimed && claimed < blocked && blocked < done);
    }

    #[test]
    fn rows_carry_state_glyph_repo_number_title() {
        let screen = render(&App::new(fixture_map()));
        assert!(screen.contains("○ wayfinder  #6    Re-entry breadcrumbs"));
        assert!(screen.contains("◐ wayfinder  #9    Main screen design"));
        assert!(screen.contains("⊘ wayfinder  #7    Supervising AFK agents"));
        assert!(screen.contains("● wayfinder  #2    Choose the stack"));
    }

    #[test]
    fn blocked_rows_show_needs_suffix() {
        let screen = render(&App::new(fixture_map()));
        assert!(screen.contains("#7    Supervising AFK agents  — needs #6"));
        assert!(screen.contains("#14   Breadcrumb markers  — needs #6, #9"));
    }

    #[test]
    fn bottom_chrome_has_count_prompt_and_hint_lines() {
        let screen = render(&App::new(fixture_map()));
        assert!(screen.contains("5/5"));
        assert!(screen.contains("> █"));
        assert!(screen.contains("enter launch"));
        assert!(screen.contains("ctrl-r refresh"));
        assert!(screen.contains("esc quit"));
        assert!(screen.contains("wf · blooop/wayfinder"));
    }

    #[test]
    fn query_drops_nonmatching_rows_but_groups_persist() {
        let mut app = App::new(fixture_map());
        type_str(&mut app, "bread");
        let screen = render(&app);
        // Surviving rows: #6 (frontier) and #14 (blocked).
        assert!(screen.contains("#6    Re-entry breadcrumbs"));
        assert!(screen.contains("#14   Breadcrumb markers"));
        // Dropped rows are gone.
        assert!(!screen.contains("Main screen design"));
        assert!(!screen.contains("Choose the stack"));
        assert!(!screen.contains("Supervising AFK agents"));
        // Every group header persists, as matched/total — 2a, no flattening.
        let frontier = screen
            .find("FRONTIER — ready to claim — 1/1")
            .expect("frontier header");
        let claimed = screen.find("CLAIMED — 0/1").expect("claimed header");
        let blocked = screen.find("BLOCKED — 1/2").expect("blocked header");
        let done = screen.find("DONE — 0/1").expect("done header");
        assert!(frontier < claimed && claimed < blocked && blocked < done);
        // Count line and prompt reflect the live query.
        assert!(screen.contains("2/5"));
        assert!(screen.contains("> bread█"));
    }

    #[test]
    fn cursor_marks_first_visible_row_after_typing() {
        let mut app = App::new(fixture_map());
        // Park the cursor lower first; typing must snap it to the first hit.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        type_str(&mut app, "bread");
        let screen = render(&app);
        assert!(screen.contains("▶ ○ wayfinder  #6    Re-entry breadcrumbs"));
        assert!(!screen.contains("▶ ⊘"));
    }

    #[test]
    fn cursor_moves_across_visible_rows_only() {
        let mut app = App::new(fixture_map());
        type_str(&mut app, "bread");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app);
        // Cursor skipped the emptied CLAIMED group straight to blocked #14.
        assert!(screen.contains("▶ ⊘ wayfinder  #14   Breadcrumb markers"));
    }

    /// The fixture map plus Build 4 launch inputs: two checkouts of the repo,
    /// so `enter` opens the which-checkout picker.
    fn launchable_app() -> App {
        let checkout = |path: &str, session: &str| crate::projects::Checkout {
            path: std::path::PathBuf::from(path),
            repo: "blooop/wayfinder".to_string(),
            session: session.to_string(),
        };
        let mut map_issues = crate::launch::MapIssues::new();
        map_issues.insert("blooop/wayfinder".to_string(), 1);
        App::new(fixture_map()).with_projects(
            vec![
                checkout("/data/k1/wayfinder", "k1"),
                checkout("/data/k2/wayfinder", "k2"),
            ],
            map_issues,
        )
    }

    #[test]
    fn the_hint_line_advertises_both_launch_keys() {
        let screen = render(&App::new(fixture_map()));
        assert!(screen.contains("enter launch"));
        assert!(screen.contains("ctrl-a afk"));
    }

    #[test]
    fn the_afk_slot_is_empty_until_agent_tabs_exist() {
        let mut app = App::new(fixture_map());
        assert_eq!(afk_line(&app), "");
        assert!(!render(&app).contains("agents:"));
        app.agent_tabs = 1;
        assert_eq!(afk_line(&app), "  agents: 1 tab");
        app.agent_tabs = 3;
        assert!(render(&app).contains("agents: 3 tabs"));
    }

    #[test]
    fn the_checkout_picker_floats_over_the_list_with_one_row_per_session() {
        let mut app = launchable_app();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("which checkout hosts wayfinder#6?"), "{screen}");
        assert!(screen.contains("▶ k1"));
        assert!(screen.contains("/data/k1/wayfinder"));
        assert!(screen.contains("  k2"));
        assert!(screen.contains("esc cancel"));
        // It is a modal: the rows it covers are overwritten, not blended.
        assert!(!screen.contains("Supervising AFK agents"), "{screen}");
    }

    #[test]
    fn count_line_carries_the_refresh_indicator() {
        let screen = render_with_indicator(&App::new(fixture_map()), "· ↻ 3s ago");
        assert!(screen.contains("5/5  · ↻ 3s ago"));
    }

    #[test]
    fn focus_mode_drops_repo_column_and_names_scope_in_title() {
        let mut app = App::new(fixture_map());
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let screen = render(&app);
        assert!(screen.contains("wf · blooop/wayfinder — focused"));
        assert!(screen.contains("ctrl-g all projects"));
        assert!(screen.contains("▶ ○ #6    Re-entry breadcrumbs"));
        assert!(!screen.contains("○ wayfinder  #6"));
    }
}
