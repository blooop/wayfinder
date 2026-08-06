//! The main screen: one cluster per open map (#50) — a `▌ repo · map title`
//! header carrying the per-status counts, with the map's tickets grouped
//! beneath it (frontier / claimed / blocked / done) in the minimal row —
//! state glyph, number, title, `— needs #N` on blocked rows. The repo column
//! is gone: every row sits under a header that names its repo. Build 2's
//! fuzzy query (2a: groups survive typing), the cursor, and the bottom prompt
//! are unchanged.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Overlay, Row, Scope};
use crate::model::{Map, MapId, Status, GROUP_LABELS};

fn glyph_style(status: &Status) -> Style {
    match status {
        Status::Frontier => Style::new().fg(Color::Green),
        Status::Claimed => Style::new().fg(Color::Yellow),
        Status::Blocked { .. } => Style::new().fg(Color::Red),
        Status::Done => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The cluster header: `▌ <repo> · <map title>  ○n ◐n ⊘n ●n`. The counts are
/// the whole map's, not the query's — they describe the cluster's shape, and
/// the group headers already carry `matched/total` while a query is live.
fn cluster_header(id: &MapId, map: &Map) -> Line<'static> {
    let [frontier, claimed, blocked, done] = map.counts();
    let count_style = [
        Style::new().fg(Color::Green),
        Style::new().fg(Color::Yellow),
        Style::new().fg(Color::Red),
        Style::new().add_modifier(Modifier::DIM),
    ];
    let glyphs = ['○', '◐', '⊘', '●'];
    let mut spans = vec![Span::styled(
        format!("▌ {} · {}", id.short_repo(), map.title),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )];
    for (i, count) in [frontier, claimed, blocked, done].into_iter().enumerate() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("{}{count}", glyphs[i]), count_style[i]));
    }
    Line::from(spans)
}

/// The cluster body as styled lines: one header per in-scope map, its groups
/// beneath it in fixed order. Shared by the live draw and the TestBackend
/// tests.
///
/// Filtering is 2a per the #9 resolution: non-matching rows drop, group
/// headers stay (showing `matched/total` while a query is live), and group
/// order never changes. A group the cluster has no tickets in at all is
/// skipped — with several clusters on screen, empty headers would outnumber
/// the rows.
pub fn body_lines(app: &App) -> Vec<Line<'static>> {
    body_with_cursor(app).0
}

/// [`body_lines`] plus which line the cursor row landed on — what the draw
/// scrolls to. `None` when no row is visible. Needed since #50: several
/// clusters can be taller than the screen, and a cursor the body cannot show
/// is a picker that cannot pick.
fn body_with_cursor(app: &App) -> (Vec<Line<'static>>, Option<usize>) {
    let visible = app.visible();
    let cursor_row = visible.get(app.cursor_pos()).cloned();
    let mut cursor_line = None;
    let filtering = !app.query.is_empty();

    let mut lines = vec![Line::default()];
    for (id, map) in app.scoped_clusters() {
        lines.push(cluster_header(id, map));
        for (group, label) in GROUP_LABELS.iter().enumerate() {
            let total = map
                .tickets
                .iter()
                .filter(|t| t.status.group() == group)
                .count();
            if total == 0 {
                continue;
            }
            let members: Vec<&Row> = visible
                .iter()
                .filter(|(row_id, i)| row_id == id && map.tickets[*i].status.group() == group)
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
            for row in members {
                let ticket = &map.tickets[row.1];
                let under_cursor = cursor_row.as_ref() == Some(row);
                if under_cursor {
                    cursor_line = Some(lines.len());
                }
                let cursor = if under_cursor { '▶' } else { ' ' };
                let mut spans = vec![
                    Span::raw(format!("  {cursor} ")),
                    Span::styled(ticket.status.glyph().to_string(), glyph_style(&ticket.status)),
                    Span::raw(format!(" #{:<4} {}", ticket.number, ticket.title)),
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
        }
        lines.push(Line::default());
    }
    lines.pop();
    (lines, cursor_line)
}

/// The keybindings (#14): `tab` peek is deferred; `enter` runs the ticket's
/// agent right here and `wf` is gone (#34); `esc` clears the query first and
/// quits on an empty one, and `q` only quits when the query is empty (mid-query
/// it types).
const KEY_HINTS: &str =
    "  enter launch · ctrl-f focus · ctrl-g all · ctrl-r refresh · esc quit";

/// The project heading in the title bar.
///
/// An empty screen has three meanings — still loading (#27), every fetch
/// failed, or genuinely no projects — and telling a user with three registered
/// projects that they have none is the exact ambiguity
/// [`crate::refresh::Startup`] exists to remove. So both other cases are named
/// before "no projects" is claimed. With clusters on screen, the heading
/// counts the ground they cover: the one repo's slug, or how many projects.
pub fn heading(app: &App) -> String {
    if app.clusters.is_empty() {
        if !app.startup.is_loaded() {
            return "loading…".to_string();
        }
        // Naming the map is the whole value when there is one: "GitHub is
        // unreachable" and "you have no projects" are different problems with
        // different fixes, and the empty list looks identical either way.
        match app.failed.len() {
            0 => {}
            1 => {
                let id = app.failed.iter().next().expect("len checked");
                return format!("{}#{} — fetch failed, ctrl-r retries", id.repo, id.number);
            }
            n => return format!("{n} maps failed to fetch — ctrl-r retries"),
        }
        return "no projects — run wf inside a checkout to register it".to_string();
    }
    let repos: std::collections::BTreeSet<&str> =
        app.clusters.keys().map(|id| id.repo.as_str()).collect();
    match repos.len() {
        1 => (*repos.iter().next().expect("len checked")).to_string(),
        n => format!("{n} projects"),
    }
}

/// The persistent failure segment on the count line.
///
/// Separate from [`heading`] because the case it exists for is the *partial*
/// one: four clusters on screen and a fifth missing draws a perfectly normal
/// screen, so the only place left to say so is here — and it has to survive
/// the next keypress, which the one-shot notice does not.
pub fn failure_note(app: &App) -> String {
    match app.failed.len() {
        0 => String::new(),
        1 => {
            let id = app.failed.iter().next().expect("len checked");
            format!("· {}#{} failed", id.repo, id.number)
        }
        n => format!("· {n} maps failed"),
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

/// The which-checkout modal: one row per registered checkout of the repo.
///
/// The one prompt `wf` still has, and the reason it survived the Build 7
/// deletion (#34): a repo can have several checkouts, the agent must run in
/// exactly one, and `wf` cannot guess which. The path *is* the row — it is what
/// distinguishes the candidates, and with no session to name there is nothing
/// shorter to show alongside it.
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
                launch.cwd().display().to_string(),
                Style::new().fg(Color::Cyan),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  enter launch here · ↑/↓ pick · esc cancel",
        Style::new().add_modifier(Modifier::DIM),
    ));
    // This asks *which checkout*, so the ticket only needs identifying — its
    // title is already on the row behind the prompt.
    let key = launches.first().map(|l| l.key()).unwrap_or_default();
    let width = lines
        .iter()
        .map(|l| l.width() as u16 + 4)
        .chain(std::iter::once(key.len() as u16 + 30))
        .max()
        .unwrap_or(40);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(format!(" which checkout runs {key}? "))
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

/// Draw the full screen: bordered frame with the scope in the title, the
/// clusters, then the anchored bottom chrome — the match-count line (with the
/// load hint and any one-shot notice), the fzf-style prompt, and the key hints.
/// The which-checkout picker, when open, floats over all of it.
pub fn draw(frame: &mut Frame, app: &App) {
    let mut block = match &app.scope {
        Scope::All => Block::bordered().title(format!(" wf · {} ", heading(app))),
        // The focused title names the project by its full slug — with one
        // project on screen there is room, and it disambiguates forks.
        Scope::Project(repo) => Block::bordered().title(format!(" wf · {repo} — focused ")),
    };
    if app.scope != Scope::All {
        block = block.title_top(Line::from(" ctrl-g all projects ").right_aligned());
    }
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let [body_area, count_area, prompt_area, hint_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // Scroll only as far as it takes to keep the cursor's line on screen —
    // several clusters can be taller than the body area (#50), and a cursor
    // below the fold would leave the picker unable to show what `enter` picks.
    let (lines, cursor_line) = body_with_cursor(app);
    let mut body = Paragraph::new(lines);
    if let Some(line) = cursor_line {
        let height = body_area.height as usize;
        if height > 0 && line >= height {
            body = body.scroll(((line + 1 - height) as u16, 0));
        }
    }
    frame.render_widget(body, body_area);

    // The load hint and the failure note share one dim segment and can both be
    // live at once (map 2 of 3 is still coming *and* map 1 never arrived);
    // empty ones drop out rather than leaving gaps.
    let status = [app.startup.hint(), failure_note(app)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut count_spans = vec![
        Span::raw(format!("  {}/{}", app.visible().len(), app.scoped().len())),
        Span::styled(
            format!("  {status}"),
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
    use crate::model::{classify, Map, MapId, MapSet, Ticket, TicketType};
    use crate::refresh::Startup;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use std::collections::BTreeMap;

    fn ticket(
        repo: &str,
        number: u64,
        title: &str,
        open: bool,
        assigned: bool,
        needs: Vec<u64>,
    ) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: title.to_string(),
            status: classify(open, assigned, needs.clone()),
            ticket_type: TicketType::Task,
            blocked_by: needs,
        }
    }

    fn wf_map() -> Map {
        let t = |number, title: &str, open, assigned, needs: Vec<u64>| {
            ticket("blooop/wayfinder", number, title, open, assigned, needs)
        };
        Map {
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

    fn fixture_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        App::new(clusters)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Render the app through TestBackend and return the screen as text.
    fn render(app: &App) -> String {
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
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
    fn the_cluster_header_names_the_map_and_carries_the_counts() {
        let screen = render(&fixture_app());
        assert!(
            screen.contains("▌ wayfinder · Map: wf  ○1  ◐1  ⊘2  ●1"),
            "{screen}"
        );
    }

    #[test]
    fn groups_render_in_fixed_order_within_the_cluster() {
        let screen = render(&fixture_app());
        let cluster = screen.find("▌ wayfinder").expect("cluster header");
        let frontier = screen.find("FRONTIER — ready to claim — 1").expect("frontier header");
        let claimed = screen.find("CLAIMED — 1").expect("claimed header");
        let blocked = screen.find("BLOCKED — 2").expect("blocked header");
        let done = screen.find("DONE — 1").expect("done header");
        assert!(cluster < frontier && frontier < claimed && claimed < blocked && blocked < done);
    }

    #[test]
    fn every_open_map_renders_as_its_own_cluster_even_on_one_repo() {
        // The #50 acceptance case: two open maps on one repo are two clusters,
        // where the old lowest-number rule showed only one.
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: the selection view".to_string(),
                tickets: vec![ticket(
                    "blooop/wayfinder",
                    50,
                    "Build: clusters",
                    true,
                    false,
                    vec![],
                )],
            },
        );
        let app = App::new(clusters);
        let screen = render(&app);
        let first = screen.find("▌ wayfinder · Map: wf").expect("map #1's cluster");
        let second = screen
            .find("▌ wayfinder · Map: the selection view")
            .expect("map #47's cluster");
        assert!(first < second, "clusters order by (repo, number)");
        assert!(screen.contains("#50   Build: clusters"), "{screen}");
        // One repo on screen, however many maps: the title names the repo.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
    }

    #[test]
    fn rows_carry_state_glyph_number_title_without_a_repo_column() {
        // The repo column is gone: the cluster header names the repo.
        let screen = render(&fixture_app());
        assert!(screen.contains("○ #6    Re-entry breadcrumbs"), "{screen}");
        assert!(screen.contains("◐ #9    Main screen design"), "{screen}");
        assert!(screen.contains("⊘ #7    Supervising AFK agents"), "{screen}");
        assert!(screen.contains("● #2    Choose the stack"), "{screen}");
        assert!(!screen.contains("○ wayfinder"), "{screen}");
    }

    #[test]
    fn blocked_rows_show_needs_suffix() {
        let screen = render(&fixture_app());
        assert!(screen.contains("#7    Supervising AFK agents  — needs #6"));
        assert!(screen.contains("#14   Breadcrumb markers  — needs #6, #9"));
    }

    #[test]
    fn bottom_chrome_has_count_prompt_and_hint_lines() {
        let screen = render(&fixture_app());
        assert!(screen.contains("5/5"));
        assert!(screen.contains("> █"));
        assert!(screen.contains("enter launch"));
        assert!(screen.contains("ctrl-r refresh"));
        assert!(screen.contains("esc quit"));
        assert!(screen.contains("wf · blooop/wayfinder"));
    }

    #[test]
    fn query_drops_nonmatching_rows_but_groups_persist() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        let screen = render(&app);
        // Surviving rows: #6 (frontier) and #14 (blocked).
        assert!(screen.contains("#6    Re-entry breadcrumbs"));
        assert!(screen.contains("#14   Breadcrumb markers"));
        // Dropped rows are gone.
        assert!(!screen.contains("Main screen design"));
        assert!(!screen.contains("Choose the stack"));
        assert!(!screen.contains("Supervising AFK agents"));
        // Group headers persist as matched/total — 2a, no flattening — and the
        // cluster header stays put above them.
        let cluster = screen.find("▌ wayfinder").expect("cluster header");
        let frontier = screen
            .find("FRONTIER — ready to claim — 1/1")
            .expect("frontier header");
        let claimed = screen.find("CLAIMED — 0/1").expect("claimed header");
        let blocked = screen.find("BLOCKED — 1/2").expect("blocked header");
        let done = screen.find("DONE — 0/1").expect("done header");
        assert!(cluster < frontier && frontier < claimed && claimed < blocked && blocked < done);
        // Count line and prompt reflect the live query.
        assert!(screen.contains("2/5"));
        assert!(screen.contains("> bread█"));
    }

    #[test]
    fn cursor_marks_first_visible_row_after_typing() {
        let mut app = fixture_app();
        // Park the cursor lower first; typing must snap it to the first hit.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        type_str(&mut app, "bread");
        let screen = render(&app);
        assert!(screen.contains("▶ ○ #6    Re-entry breadcrumbs"));
        assert!(!screen.contains("▶ ⊘"));
    }

    #[test]
    fn cursor_moves_across_visible_rows_only() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app);
        // Cursor skipped the emptied CLAIMED group straight to blocked #14.
        assert!(screen.contains("▶ ⊘ #14   Breadcrumb markers"));
    }

    /// The fixture map plus Build 4 launch inputs: two checkouts of the repo,
    /// so `enter` opens the which-checkout picker.
    fn launchable_app() -> App {
        let checkout = |path: &str| crate::projects::Checkout {
            path: std::path::PathBuf::from(path),
            repo: "blooop/wayfinder".to_string(),
        };
        fixture_app().with_checkouts(vec![
            checkout("/data/k1/wayfinder"),
            checkout("/data/k2/wayfinder"),
        ])
    }

    #[test]
    fn the_hint_line_advertises_the_one_launch_key_and_no_afk() {
        let screen = render(&fixture_app());
        assert!(screen.contains("enter launch"));
        assert!(!screen.contains("ctrl-a"), "{screen}");
        assert!(!screen.contains("afk"), "{screen}");
    }

    #[test]
    fn nothing_reserves_a_line_for_agents_any_more() {
        // The `agents: N tabs` slot went with the tabs it counted (#26).
        let screen = render(&fixture_app());
        assert!(!screen.contains("agents:"), "{screen}");
    }

    #[test]
    fn the_checkout_picker_floats_over_the_list_with_one_row_per_tree() {
        let mut app = launchable_app();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("which checkout runs wayfinder#6?"), "{screen}");
        assert!(screen.contains("▶ /data/k1/wayfinder"), "{screen}");
        assert!(screen.contains("/data/k2/wayfinder"), "{screen}");
        assert!(screen.contains("esc cancel"));
        // It is a modal: the rows it covers are overwritten, not blended.
        assert!(!screen.contains("Breadcrumb markers"), "{screen}");
    }

    #[test]
    fn the_first_frame_says_it_is_loading_rather_than_that_there_is_nothing() {
        // The whole point of #27: this screen is drawn before any network call,
        // so its empty list must not read as "no tickets" or "no projects".
        let screen = render(&App::empty());
        assert!(screen.contains("searching for maps…"), "{screen}");
        assert!(screen.contains("wf · loading…"), "{screen}");
        assert!(!screen.contains("no projects"), "{screen}");
    }

    #[test]
    fn an_empty_list_after_a_failed_fetch_does_not_claim_there_are_no_projects() {
        // Three registered projects and GitHub unreachable draws the same empty
        // list as no projects at all. Saying "no projects — run wf inside a
        // checkout" there sends the user to fix the one thing that is not
        // broken, so the failure has to win the heading — and it names the
        // *map*, because with several on one repo the repo alone is ambiguous.
        let mut app = App::empty();
        app.startup = Startup::loaded();
        app.failed.insert(MapId::new("blooop/wayfinder", 35));
        let screen = render(&app);
        assert!(!screen.contains("no projects"), "{screen}");
        assert!(
            screen.contains("blooop/wayfinder#35 — fetch failed, ctrl-r retries"),
            "{screen}"
        );

        // Several, and naming them all would not fit: say how many.
        app.failed.insert(MapId::new("blooop/dotfiles", 4));
        let screen = render(&app);
        assert!(screen.contains("2 maps failed to fetch"), "{screen}");
        assert!(!screen.contains("no projects"), "{screen}");
    }

    #[test]
    fn a_partial_failure_is_visible_and_survives_the_next_keypress() {
        // The case that hides best: the clusters that did load look completely
        // normal, so the count line is the only place left to say a map is
        // missing — and `notice` cannot be that place, because `handle_key`
        // clears it on every keypress and nothing polls to re-announce it.
        let mut app = fixture_app();
        app.failed.insert(MapId::new("blooop/dotfiles", 4));
        assert_eq!(failure_note(&app), "· blooop/dotfiles#4 failed");
        let screen = render(&app);
        assert!(screen.contains("5/5  · blooop/dotfiles#4 failed"), "{screen}");

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let screen = render(&app);
        assert!(
            screen.contains("· blooop/dotfiles#4 failed"),
            "a keypress must not erase it: {screen}"
        );
    }

    #[test]
    fn an_empty_list_reads_as_empty_only_once_the_load_finished() {
        // Same empty screen, opposite meaning — told apart by Startup alone.
        let mut app = App::empty();
        app.startup = Startup::loaded();
        let screen = render(&app);
        assert!(screen.contains("no projects — run wf inside a checkout"), "{screen}");
        assert!(!screen.contains("loading"), "{screen}");
    }

    #[test]
    fn a_partial_load_shows_progress_beside_the_clusters_already_in() {
        // One map of three has landed: the rows are real and fresh, and the
        // count line still says more is coming.
        let mut app = fixture_app();
        let found: MapSet = [
            MapId::new("blooop/wayfinder", 1),
            MapId::new("b/two", 2),
            MapId::new("c/three", 3),
        ]
        .into_iter()
        .collect();
        app.startup = Startup::seeded(&found);
        app.startup.searched(&found);
        app.startup.record_arrival(&MapId::new("blooop/wayfinder", 1));
        let screen = render(&app);
        assert!(screen.contains("#6    Re-entry breadcrumbs"), "{screen}");
        assert!(screen.contains("5/5  · loading maps 1/3"), "{screen}");
        // Rows exist, so the title names the project rather than the wait.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
    }

    #[test]
    fn the_body_scrolls_to_keep_the_cursor_on_screen() {
        // One cluster of 30 done tickets ahead of a one-ticket cluster: the
        // second cluster's rows start past the 24-row screen, and that is where
        // the cursor must still be *visible* — a picker that cannot show what
        // `enter` would pick is broken (the live 3-map repo hits exactly this).
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                tickets: (1..=30)
                    .map(|n| ticket("blooop/wayfinder", n, &format!("Done {n}"), false, false, vec![]))
                    .collect(),
            },
        );
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: the selection view".to_string(),
                tickets: vec![ticket("blooop/wayfinder", 50, "Build: clusters", true, false, vec![])],
            },
        );
        let mut app = App::new(clusters);
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.cursor_ticket().unwrap().number, 50);
        let screen = render(&app);
        assert!(screen.contains("▶ ○ #50   Build: clusters"), "{screen}");
        // And with the cursor back at the top, the top is what shows.
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        let screen = render(&app);
        assert!(screen.contains("▶ ● #1    Done 1"), "{screen}");
    }

    #[test]
    fn focus_mode_names_the_scope_in_the_title() {
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let screen = render(&app);
        assert!(screen.contains("wf · blooop/wayfinder — focused"));
        assert!(screen.contains("ctrl-g all projects"));
        assert!(screen.contains("▶ ○ #6    Re-entry breadcrumbs"));
    }
}
