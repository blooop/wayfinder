//! The main screen: one cluster per open map (#50), rendered from the body
//! [`Plan`] (#51). The default is the leverage view — takeable tickets,
//! most-dependents-first, each with the subtree it unblocks — with the full
//! blocking forest on `tab` and a live query flattening either into one
//! score-ordered list. Rows are `<glyph> #n <title> [type] ⇄ PR#n <state>`;
//! done work is a per-cluster count on the default screen and dimmed in place
//! on the forest.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Overlay, Scope};
use crate::model::{Checks, Map, MapId, PrLink, PrStatus, Review, Status, Ticket};
use crate::view::{GroupKind, Item, Plan, Screen};

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
        spans.push(Span::styled(
            format!("{}{count}", glyphs[i]),
            count_style[i],
        ));
    }
    Line::from(spans)
}

/// The cursor marker, which sits **after** a row's tree furniture and directly
/// against the item it points at.
///
/// Column matters here. Parked in a fixed left-hand gutter it could not show
/// depth at all: `→` moving the cursor a level deeper looked identical to `↓`
/// moving it a line down, so the depth axis was invisible and the keys felt
/// interchangeable. Riding with the indentation, the marker steps visibly
/// rightward as you descend, which is the whole feedback the depth keys need.
fn cursor_span(under_cursor: bool) -> Span<'static> {
    if under_cursor {
        Span::styled("▶ ", CURSOR_COLOR)
    } else {
        Span::raw("  ")
    }
}

/// One colour shared by the marker and the branch run leading into it, so the
/// selected row reads as a single lit-up path rather than a lone glyph.
const CURSOR_COLOR: Style = Style::new().fg(Color::Cyan);

/// How a row's tree furniture is drawn: dim on every ordinary row, and in the
/// cursor's own colour on the selected one.
///
/// The branches are what say *where* the cursor is. Left uniformly dim, the
/// marker was the only lit thing on a screen full of near-identical indented
/// rows; lighting the run of furniture up to it draws the eye along the path
/// from the parent down to the selection.
fn furniture_style(under_cursor: bool) -> Style {
    if under_cursor {
        CURSOR_COLOR
    } else {
        Style::new().add_modifier(Modifier::DIM)
    }
}

/// The `⇄ PR#n <state>` badge spans for one linked PR (#52) — evidence of the
/// ticket's progress, riding after the `[type]` suffix. An open PR folds its
/// two live signals into one glyph: `✗` when something needs acting on (checks
/// failing or changes requested), `✓` when nothing is outstanding, nothing
/// while it is still in flight. A PR living in another repo names it — links
/// are cross-repo capable and the badge must not imply otherwise.
fn pr_badge(ticket_repo: &str, pr: &PrLink) -> Vec<Span<'static>> {
    let repo = if pr.repo == ticket_repo {
        String::new()
    } else {
        format!(" {}", pr.short_repo())
    };
    let (word, verdict) = match &pr.status {
        PrStatus::Draft => ("draft", None),
        PrStatus::Merged => ("merged", None),
        PrStatus::Closed => ("closed", None),
        PrStatus::Open { checks, review } => {
            let acting_needed =
                matches!(checks, Checks::Failing) || matches!(review, Review::ChangesRequested);
            let settled = matches!(checks, Checks::Passing | Checks::Absent)
                && matches!(review, Review::Approved | Review::NotRequired);
            let verdict = if acting_needed {
                Some(Span::styled(" ✗", Style::new().fg(Color::Red)))
            } else if settled {
                Some(Span::styled(" ✓", Style::new().fg(Color::Green)))
            } else {
                None
            };
            ("open", verdict)
        }
    };
    let mut spans = vec![Span::styled(
        format!("  ⇄ PR{repo}#{} {word}", pr.number),
        Style::new().fg(Color::Magenta),
    )];
    spans.extend(verdict);
    spans
}

/// One ticket row: cursor marker, tree furniture, state glyph, `#n title`,
/// then the dim `[type]` suffix and any `⇄ PR` badges — and on the forest, the
/// extra blocking edges the tree position cannot show (`⤷ also needs #n`). The
/// flattened screen has no cluster header above the row, so the row names its
/// repo itself.
fn ticket_line(
    ticket: &Ticket,
    prefix: &str,
    also_needs: &[u64],
    name_repo: bool,
    under_cursor: bool,
) -> Line<'static> {
    let repo = if name_repo {
        ticket.short_repo().to_string()
    } else {
        String::new()
    };
    // Nested rows carry the cursor column as extra indent, so a branch begins
    // directly under the glyph of the row it hangs from instead of to its left.
    let indent = if prefix.is_empty() {
        String::new()
    } else {
        format!("  {prefix}")
    };
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(indent, furniture_style(under_cursor)),
        cursor_span(under_cursor),
        Span::styled(
            ticket.status.glyph().to_string(),
            glyph_style(&ticket.status),
        ),
        Span::raw(format!(" {repo}#{} {}", ticket.number, ticket.title)),
    ];
    if let Some(name) = ticket.ticket_type.short_name() {
        spans.push(Span::styled(
            format!(" [{name}]"),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    for pr in &ticket.prs {
        spans.extend(pr_badge(&ticket.repo, pr));
    }
    if !also_needs.is_empty() {
        let needs: Vec<String> = also_needs.iter().map(|n| format!("#{n}")).collect();
        spans.push(Span::styled(
            format!("  ⤷ also needs {}", needs.join(", ")),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    let style = match ticket.status {
        Status::Done => Style::new().add_modifier(Modifier::DIM),
        _ => Style::new(),
    };
    Line::from(spans).style(style)
}

/// The body as styled lines: the [`Plan`] walked in order. Shared by the live
/// draw and the TestBackend tests.
pub fn body_lines(app: &App) -> Vec<Line<'static>> {
    body_with_cursor(app, &app.plan()).0
}

/// A collapsible group's line (#57): the cursor column, a `▸`/`▾` fold marker
/// where a ticket row's tree furniture would be, then the count it is holding.
/// It says `(hidden)` only while shut — once open, the rows are right there and
/// claiming otherwise would be a lie.
fn group_line(kind: GroupKind, hidden: usize, expanded: bool, under_cursor: bool) -> Line<'static> {
    let fold = if expanded { '▾' } else { '▸' };
    let (glyph, label, color) = match kind {
        GroupKind::BlockedDeeper => ('⊘', "blocked deeper down", Color::Red),
        GroupKind::Done => ('●', "done", Color::Reset),
    };
    let tail = if expanded { "" } else { " (hidden)" };
    Line::from(vec![
        Span::raw("  "),
        cursor_span(under_cursor),
        Span::styled(format!("{fold} "), furniture_style(under_cursor)),
        Span::styled(glyph.to_string(), Style::new().fg(color)),
        Span::styled(
            format!(" {hidden} {label}{tail}"),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ])
}

/// [`body_lines`] plus which line the cursor landed on — what the draw scrolls
/// to. `None` when nothing is on screen. Needed since #50: several clusters can
/// be taller than the screen, and a cursor the body cannot show is a picker
/// that cannot pick.
///
/// The cursor is matched by *stop position*, not identity: the leverage view
/// can legitimately show one ticket twice (as a takeable root and inside
/// another root's subtree), and only one of those occurrences is under the
/// cursor.
fn body_with_cursor(app: &App, plan: &Plan) -> (Vec<Line<'static>>, Option<usize>) {
    let name_repo = matches!(app.screen(), Screen::Flattened { .. });
    let cursor_pos = app.cursor_pos();
    let mut stop = 0usize;
    let mut cursor_line = None;
    // Every stop the plan lists gets one line, in the same order, so this
    // single counter is what keeps the drawn ▶ and the cursor in agreement.
    let mut mark = |lines: &Vec<Line<'static>>, cursor_line: &mut Option<usize>| {
        let under_cursor = stop == cursor_pos;
        if under_cursor {
            *cursor_line = Some(lines.len());
        }
        stop += 1;
        under_cursor
    };

    let mut lines = vec![Line::default()];
    for item in &plan.items {
        match item {
            Item::Header(id) => lines.push(cluster_header(id, &app.clusters[id])),
            Item::Ticket {
                row,
                prefix,
                also_needs,
                depth: _,
            } => {
                let under_cursor = mark(&lines, &mut cursor_line);
                lines.push(ticket_line(
                    app.ticket(row),
                    prefix,
                    also_needs,
                    name_repo,
                    under_cursor,
                ));
            }
            Item::Group {
                id,
                hidden,
                expanded,
            } => {
                let under_cursor = mark(&lines, &mut cursor_line);
                lines.push(group_line(id.kind, *hidden, *expanded, under_cursor));
            }
            Item::Blank => lines.push(Line::default()),
        }
    }
    (lines, cursor_line)
}

/// The keybindings (#14): `tab` peek is deferred; `enter` runs the ticket's
/// agent right here and `wf` is gone (#34); `esc` clears the query first and
/// quits on an empty one, and `q` only quits when the query is empty (mid-query
/// it types).
const KEY_HINTS: &str =
    "  enter launch · ←→ open · tab structure · ctrl-f focus · ctrl-r refresh · esc quit";

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

/// The `· N idle maps hidden` segment on the count line (#51): the leverage
/// view drops a map with nothing takeable from the body, and the count is the
/// only trace of it — silence would read as the map not existing at all. The
/// forest (`tab`) shows the dropped maps in full.
pub fn idle_note(plan: &Plan) -> String {
    match plan.idle_hidden {
        0 => String::new(),
        1 => "· 1 idle map hidden".to_string(),
        n => format!("· {n} idle maps hidden"),
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
    let plan = app.plan();
    let (lines, cursor_line) = body_with_cursor(app, &plan);
    let mut body = Paragraph::new(lines);
    if let Some(line) = cursor_line {
        let height = body_area.height as usize;
        if height > 0 && line >= height {
            body = body.scroll(((line + 1 - height) as u16, 0));
        }
    }
    frame.render_widget(body, body_area);

    // The load hint, the failure note, and the idle count share one dim
    // segment, and any of them can be live at once; empty ones drop out
    // rather than leaving gaps.
    let status = [app.startup.hint(), failure_note(app), idle_note(&plan)]
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
            prs: vec![],
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
    fn the_default_screen_is_the_leverage_view_not_the_groups() {
        let screen = render(&fixture_app());
        // Takeable roots, most-open-dependents-first: #6 (unblocks #7 and #14)
        // before #9 (unblocks #14) — each with its subtree drawn beneath it.
        let root6 = screen.find("○ #6 Re-entry breadcrumbs").expect("root #6");
        let sub7 = screen
            .find("├─  ⊘ #7 Supervising AFK agents")
            .expect("#7 under #6");
        let sub14 = screen
            .find("└─  ⊘ #14 Breadcrumb markers")
            .expect("#14 under #6");
        let root9 = screen.find("◐ #9 Main screen design").expect("root #9");
        assert!(root6 < sub7 && sub7 < sub14 && sub14 < root9, "{screen}");
        // Done work is a count, not rows; the group headers retired with #51.
        assert!(screen.contains("● 1 done (hidden)"), "{screen}");
        assert!(!screen.contains("Choose the stack"), "{screen}");
        assert!(!screen.contains("FRONTIER"), "{screen}");
        assert!(!screen.contains("CLAIMED"), "{screen}");
        assert!(!screen.contains("BLOCKED —"), "{screen}");
        assert!(!screen.contains("DONE"), "{screen}");
    }

    #[test]
    fn tab_shows_the_structure_forest_with_done_in_place() {
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        // The whole DAG: done #2 back in place, roots ascending (#2, #6, #9),
        // #7 and #14 as children of #6 — and #14's second blocking edge, which
        // the tree position cannot show, annotated on the row.
        let done = screen.find("● #2 Choose the stack").expect("done in place");
        let root6 = screen.find("○ #6 Re-entry breadcrumbs").expect("root #6");
        let sub7 = screen
            .find("├─  ⊘ #7 Supervising AFK agents")
            .expect("#7 under #6");
        let sub14 = screen
            .find("└─  ⊘ #14 Breadcrumb markers")
            .expect("#14 under #6");
        let root9 = screen.find("◐ #9 Main screen design").expect("root #9");
        assert!(
            done < root6 && root6 < sub7 && sub7 < sub14 && sub14 < root9,
            "{screen}"
        );
        assert!(screen.contains("⤷ also needs #9"), "{screen}");
        assert!(
            !screen.contains("(hidden)"),
            "the forest hides nothing: {screen}"
        );
        // Tab again restores the leverage view.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("● 1 done (hidden)"), "{screen}");
    }

    #[test]
    fn rows_show_the_ticket_type() {
        // The fixture tickets are all tasks; the suffix is on every row.
        let screen = render(&fixture_app());
        assert!(
            screen.contains("#6 Re-entry breadcrumbs [task]"),
            "{screen}"
        );
    }

    #[test]
    fn pr_badges_ride_their_ticket_row() {
        let mut map = wf_map();
        let t6 = map
            .tickets
            .iter_mut()
            .find(|t| t.number == 6)
            .expect("#6 in the fixture");
        t6.prs = vec![
            PrLink {
                repo: "blooop/wayfinder".to_string(),
                number: 46,
                status: PrStatus::Merged,
            },
            // Cross-repo, and with both live signals demanding action.
            PrLink {
                repo: "blooop/dotfiles".to_string(),
                number: 12,
                status: PrStatus::Open {
                    checks: Checks::Failing,
                    review: Review::ChangesRequested,
                },
            },
        ];
        // Nothing outstanding on #9's PR: checks pass, no review required.
        map.tickets
            .iter_mut()
            .find(|t| t.number == 9)
            .expect("#9 in the fixture")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 13,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::NotRequired,
            },
        }];
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let app = App::new(clusters);
        let screen = render(&app);
        // Same-repo badge: `⇄ PR#n <state>` after the [type] suffix.
        assert!(
            screen.contains("Re-entry breadcrumbs [task]  ⇄ PR#46 merged"),
            "{screen}"
        );
        // Cross-repo badge names the PR's repo; ✗ folds failing checks and a
        // changes-requested review into one act-on-it signal.
        assert!(screen.contains("⇄ PR dotfiles#12 open ✗"), "{screen}");
        // ✓ only when nothing is outstanding.
        assert!(screen.contains("⇄ PR#13 open ✓"), "{screen}");
    }

    #[test]
    fn an_in_flight_open_pr_gets_no_verdict_glyph() {
        let mut map = wf_map();
        map.tickets
            .iter_mut()
            .find(|t| t.number == 6)
            .expect("#6")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 14,
            status: PrStatus::Open {
                checks: Checks::Pending,
                review: Review::Required,
            },
        }];
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let screen = render(&App::new(clusters));
        assert!(screen.contains("⇄ PR#14 open"), "{screen}");
        assert!(!screen.contains('✓'), "{screen}");
        assert!(!screen.contains('✗'), "{screen}");
    }

    #[test]
    fn the_branch_leading_into_the_cursor_row_is_lit_not_dim() {
        // On a screen of near-identical indented rows the lone ▶ was doing all
        // the work; the run of furniture into it is what shows *where* the
        // selection sits, so it shares the marker's colour.
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.cursor_ticket().expect("a ticket").number, 7);

        let lines = body_lines(&app);
        let furniture = |needle: &str| -> Style {
            let line = lines
                .iter()
                .find(|line| line.to_string().contains(needle))
                .unwrap_or_else(|| panic!("no row for {needle}"));
            line.spans
                .iter()
                .find(|span| span.content.contains('─'))
                .unwrap_or_else(|| panic!("no branch on {needle}"))
                .style
        };

        assert_eq!(
            furniture("#7 Supervising").fg,
            Some(Color::Cyan),
            "the selected row's branch is lit"
        );
        let elsewhere = furniture("#14 Breadcrumb");
        assert_ne!(elsewhere.fg, Some(Color::Cyan));
        assert!(
            elsewhere.add_modifier.contains(Modifier::DIM),
            "every other branch stays dim"
        );
    }

    #[test]
    fn a_group_line_shows_its_fold_state_and_takes_the_cursor() {
        let mut app = fixture_app();
        let screen = render(&app);
        // Shut: a `▸` fold marker and the count, saying it is hiding them.
        assert!(screen.contains("▸ ● 1 done (hidden)"), "{screen}");

        // Walk onto it — it is an ordinary stop, so the ▶ lands on it and the
        // cursor column lines up with the ticket rows above.
        while !matches!(app.cursor_stop(), Some(crate::view::Stop::Group(_))) {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let screen = render(&app);
        assert!(screen.contains("▶ ▸ ● 1 done (hidden)"), "{screen}");

        // Open: `▾`, and "(hidden)" drops — the row is right there now.
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▶ ▾ ● 1 done"), "{screen}");
        assert!(!screen.contains("(hidden)"), "{screen}");
        assert!(
            screen.contains("└─  ● #2 Choose the stack"),
            "the done ticket hangs off the group line: {screen}"
        );
    }

    #[test]
    fn idle_maps_drop_to_the_count_line_and_tab_brings_them_back() {
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        clusters.insert(
            MapId::new("blooop/dotfiles", 4),
            Map {
                title: "Map: dotfiles".to_string(),
                tickets: vec![ticket(
                    "blooop/dotfiles",
                    103,
                    "All done here",
                    false,
                    false,
                    vec![],
                )],
            },
        );
        let mut app = App::new(clusters);
        let screen = render(&app);
        assert!(
            !screen.contains("▌ dotfiles"),
            "an idle map leaves the body: {screen}"
        );
        assert!(screen.contains("· 1 idle map hidden"), "{screen}");
        // The forest is the escape hatch: everything renders there.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▌ dotfiles · Map: dotfiles"), "{screen}");
        assert!(!screen.contains("idle map"), "{screen}");
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
        let first = screen
            .find("▌ wayfinder · Map: wf")
            .expect("map #1's cluster");
        let second = screen
            .find("▌ wayfinder · Map: the selection view")
            .expect("map #47's cluster");
        assert!(first < second, "clusters order by (repo, number)");
        assert!(screen.contains("#50 Build: clusters"), "{screen}");
        // One repo on screen, however many maps: the title names the repo.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
    }

    #[test]
    fn rows_carry_state_glyph_number_title_without_a_repo_column() {
        // The repo column is gone: the cluster header names the repo, and the
        // structured rows never repeat it.
        let screen = render(&fixture_app());
        assert!(screen.contains("○ #6 Re-entry breadcrumbs"), "{screen}");
        assert!(screen.contains("◐ #9 Main screen design"), "{screen}");
        assert!(screen.contains("⊘ #7 Supervising AFK agents"), "{screen}");
        assert!(!screen.contains("○ wayfinder"), "{screen}");
    }

    #[test]
    fn bottom_chrome_has_count_prompt_and_hint_lines() {
        let screen = render(&fixture_app());
        assert!(screen.contains("5/5"));
        assert!(screen.contains("> █"));
        assert!(screen.contains("enter launch"));
        assert!(screen.contains("tab structure"));
        assert!(screen.contains("ctrl-r refresh"));
        assert!(screen.contains("esc quit"));
        assert!(screen.contains("wf · blooop/wayfinder"));
    }

    #[test]
    fn a_query_flattens_to_a_scored_list_whose_rows_name_their_repo() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        let screen = render(&app);
        // Surviving rows — flat, with no cluster header above them, so each
        // names its repo itself.
        assert!(
            screen.contains("○ wayfinder#6 Re-entry breadcrumbs"),
            "{screen}"
        );
        assert!(
            screen.contains("⊘ wayfinder#14 Breadcrumb markers"),
            "{screen}"
        );
        assert!(
            !screen.contains("▌"),
            "no cluster furniture while flattened: {screen}"
        );
        assert!(!screen.contains("(hidden)"), "{screen}");
        // Dropped rows are gone.
        assert!(!screen.contains("Main screen design"));
        assert!(!screen.contains("Choose the stack"));
        assert!(!screen.contains("Supervising AFK agents"));
        // Count line and prompt reflect the live query; the denominator is
        // the map's tickets, not the leverage rows.
        assert!(screen.contains("2/5"));
        assert!(screen.contains("> bread█"));
        // Clearing the query restores the structured screen.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▌ wayfinder · Map: wf"), "{screen}");
    }

    #[test]
    fn cursor_marks_first_visible_row_after_typing() {
        let mut app = fixture_app();
        // Park the cursor lower first; typing must snap it to the first hit.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        type_str(&mut app, "bread");
        let screen = render(&app);
        assert_eq!(screen.matches('▶').count(), 1, "{screen}");
        let marked = screen
            .lines()
            .find(|l| l.contains('▶'))
            .expect("cursor row");
        assert_eq!(
            marked.contains("#6"),
            app.cursor_ticket().unwrap().number == 6,
            "the ▶ sits on the ticket the cursor names: {screen}"
        );
        assert_eq!(
            app.cursor_pos(),
            0,
            "typing snaps the cursor to the best hit"
        );
    }

    #[test]
    fn cursor_moves_across_visible_rows_only() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        let first = app.cursor_ticket().expect("a match").number;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let second = app.cursor_ticket().expect("a match").number;
        // Two matches (#6 and #14): down moves to the other, skipping every
        // ticket the query dropped.
        assert_ne!(first, second);
        assert!([6, 14].contains(&first) && [6, 14].contains(&second));
        let screen = render(&app);
        assert_eq!(screen.matches('▶').count(), 1, "{screen}");
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
        assert!(
            screen.contains("which checkout runs wayfinder#6?"),
            "{screen}"
        );
        assert!(screen.contains("▶ /data/k1/wayfinder"), "{screen}");
        assert!(screen.contains("/data/k2/wayfinder"), "{screen}");
        assert!(screen.contains("esc cancel"));
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
        assert!(
            screen.contains("5/5  · blooop/dotfiles#4 failed"),
            "{screen}"
        );

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
        assert!(
            screen.contains("no projects — run wf inside a checkout"),
            "{screen}"
        );
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
        app.startup
            .record_arrival(&MapId::new("blooop/wayfinder", 1));
        let screen = render(&app);
        assert!(screen.contains("#6 Re-entry breadcrumbs"), "{screen}");
        assert!(screen.contains("5/5  · loading maps 1/3"), "{screen}");
        // Rows exist, so the title names the project rather than the wait.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
    }

    #[test]
    fn the_body_scrolls_to_keep_the_cursor_on_screen() {
        // One cluster of 30 takeable tickets ahead of a one-ticket cluster:
        // the second cluster's rows start past the 24-row screen, and that is
        // where the cursor must still be *visible* — a picker that cannot show
        // what `enter` would pick is broken.
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                tickets: (1..=30)
                    .map(|n| {
                        ticket(
                            "blooop/wayfinder",
                            n,
                            &format!("Open {n}"),
                            true,
                            false,
                            vec![],
                        )
                    })
                    .collect(),
            },
        );
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
        let mut app = App::new(clusters);
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.cursor_ticket().unwrap().number, 50);
        let screen = render(&app);
        assert!(screen.contains("▶ ○ #50 Build: clusters"), "{screen}");
        // And with the cursor back at the top, the top is what shows.
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        let screen = render(&app);
        assert!(screen.contains("▶ ○ #1 Open 1"), "{screen}");
    }

    #[test]
    fn focus_mode_names_the_scope_in_the_title() {
        let mut app = fixture_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let screen = render(&app);
        assert!(screen.contains("wf · blooop/wayfinder — focused"));
        assert!(screen.contains("ctrl-g all projects"));
        assert!(screen.contains("▶ ○ #6 Re-entry breadcrumbs"));
    }
}
