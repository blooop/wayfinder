//! The main screen: one cluster per open map (#50), rendered from the body
//! [`Plan`] (#51). The default is the leverage view — takeable tickets,
//! most-dependents-first, each with the subtree it unblocks — with the full
//! blocking forest on `tab` and a live query sifting either one down to its
//! matches, tree and all. Rows are `<glyph> #n <title> [type] ⇄ PR#n <state>`;
//! done work is a per-cluster count on the default screen and dimmed in place
//! on the forest. A sifted screen dims whole rows: those are the ones kept only
//! to place a match, and the cursor cannot land on them.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Overlay, Scope};
use crate::filter;
use crate::launch::Launch;
use crate::model::{Checks, Map, MapId, PrLink, PrStatus, Review, RowGlyph, Stage, Status, Ticket};
use crate::view::{Branch, Fold, GroupKind, Item, Plan};

/// One colour per glyph meaning, shared by the row column and the rollup
/// pairs: calm colours for the flowing stages, red for the two that demand
/// someone (`!` act, `⊘` unblock), dim for what is finished.
fn glyph_style(glyph: RowGlyph) -> Style {
    match glyph {
        RowGlyph::Stage(Stage::Ready) => Style::new().fg(Color::Green),
        RowGlyph::Stage(Stage::Building) => Style::new().fg(Color::Yellow),
        RowGlyph::Stage(Stage::InReview) => Style::new().fg(Color::Magenta),
        // One arm, because it is one meaning: both of these are waiting on a
        // person, and the colour is what says so.
        RowGlyph::Stage(Stage::NeedsAttention) | RowGlyph::Blocked => Style::new().fg(Color::Red),
        RowGlyph::Stage(Stage::Done) => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The cluster header: `▌ <repo> · <map title>  ○n ◐n ◍n !n ●n ⊘n`. The counts
/// are the whole map's, not the query's — they describe the cluster's shape,
/// and the group headers already carry `matched/total` while a query is live.
///
/// They are **stage** counts (#78), tallied through the same [`RowGlyph`] the
/// rows below are drawn from and coloured by the same [`glyph_style`]. The
/// header used to keep its own four-status tally with its own glyph array and
/// its own colour table, which meant the same characters said different things
/// a line apart: a node the row drew `!` was counted under `○`, and `◍`/`!`
/// could not appear here at all. Glyphs the map has nobody in drop out rather
/// than showing a zero.
///
/// The repo name carries the query's match on it (`lit`), because the repo is
/// half of what a ticket is matched against and the rows below do not draw it:
/// typing a project name would otherwise sift the whole screen down to one
/// cluster while underlining nothing anywhere.
fn cluster_header(id: &MapId, map: &Map, lit: &[usize]) -> Line<'static> {
    let cyan = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled("▌ ", cyan)];
    spans.extend(lit_spans(id.short_repo(), lit, cyan));
    spans.push(Span::styled(format!(" · {}", map.title), cyan));
    for (glyph, count) in map.tally() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{}{count}", glyph.char()),
            glyph_style(glyph),
        ));
    }
    Line::from(spans)
}

/// A rollup's trailing spans: a dim word for what the counts are *of*, then
/// the glyph+count pairs in display order, each in the colour its glyph means.
///
/// One function, because a collapsed group and a branch root are asking the
/// same question of different sets of rows — what is under here, by stage — and
/// two renderings of one answer is how the screen ended up speaking two glyph
/// vocabularies in the first place (#78).
fn rollup_spans(label: &str, rollup: &[(RowGlyph, usize)]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!(" ({label})"),
        Style::new().add_modifier(Modifier::DIM),
    )];
    for &(glyph, count) in rollup {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{}{count}", glyph.char()),
            glyph_style(glyph),
        ));
    }
    spans
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
        Span::styled(
            "▶ ",
            Style::new().fg(CURSOR_COLOR).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("  ")
    }
}

/// The marker's colour: orange, and deliberately not one of the six the screen
/// already spends — cyan on cluster headers and the prompt, green/yellow/red on
/// the stage glyphs and counts, magenta on PR badges, dim on everything
/// settled. A selection drawn in any of those competes with something that means
/// something else, which is how the cursor got hard to find in the first place.
///
/// It is the *only* thing drawn in it. Lighting the branch run leading down to
/// the selection as well was tried and dropped: telling apart the elbows that
/// lead to the cursor from the guides that merely pass it needs rules that are
/// hard to see and easy to get subtly wrong, and a marker that stands out on its
/// own does the job the highlight was for.
const CURSOR_COLOR: Color = Color::Indexed(208);

/// Tree furniture is uniformly dim: it is structure, not status, and the orange
/// marker is what says where the cursor is.
const FURNITURE: Style = Style::new().add_modifier(Modifier::DIM);

/// What a live query underlines: the characters it actually landed on, bold and
/// underlined, over whatever the text was already wearing.
///
/// Deliberately not a colour. Every colour on this screen already means
/// something — cyan is a cluster, green/yellow/red are stages, magenta is a PR,
/// orange is the cursor and nothing else, dim is finished — and a match is not
/// a *kind* of thing, it is a property any of them can have: the query has to
/// be able to land on a done row inside a group and a ready row at the top of a
/// branch and say the same thing about both. A modifier composes with the
/// colour already there; a seventh colour would have had to overrule it.
const MATCHED: Modifier = Modifier::BOLD.union(Modifier::UNDERLINED);

/// `text` as spans, with the characters at `lit` wearing [`MATCHED`] over
/// `base`. Runs are coalesced, so a contiguous match is one span rather than
/// one per character. `lit` must be sorted and unique — [`filter::Hit`]
/// guarantees it — because this walks the two in step.
fn lit_spans(text: &str, lit: &[usize], base: Style) -> Vec<Span<'static>> {
    if lit.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let matched = base.add_modifier(MATCHED);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    let mut next = 0;
    for (i, ch) in text.chars().enumerate() {
        let now = lit.get(next) == Some(&i);
        if now {
            next += 1;
        }
        if !run.is_empty() && now != run_lit {
            let style = if run_lit { matched } else { base };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_lit = now;
        run.push(ch);
    }
    let style = if run_lit { matched } else { base };
    spans.push(Span::styled(run, style));
    spans
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
/// repo is never named here: every screen keeps its cluster headers, and those
/// name it once for all the rows beneath.
///
/// A row that heads a branch closes with the stage rollup of it (#62), so the
/// shape of a subtree reads off its root without walking the subtree — the
/// same glyph+count pairs a collapsed group carries, because they mean the
/// same thing.
///
/// `lit` is where a live query landed in the `#n title` half — the only part of
/// the row that was matched against, and so the only part that can honestly
/// claim to be why the row is on screen.
fn ticket_line(
    ticket: &Ticket,
    prefix: &str,
    also_needs: &[u64],
    branch: &Branch,
    lit: &[usize],
    under_cursor: bool,
) -> Line<'static> {
    // Nested rows carry the cursor column as extra indent, so a branch begins
    // directly under the glyph of the row it hangs from instead of to its left.
    let indent = if prefix.is_empty() {
        String::new()
    } else {
        format!("  {prefix}")
    };
    // The glyph column is the node's stage, `⊘` overriding when it is blocked
    // (#61/#62) — one sum type, so the row and the rollups share meanings.
    let glyph = RowGlyph::of(ticket);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(indent, FURNITURE),
        cursor_span(under_cursor),
        Span::styled(glyph.char().to_string(), glyph_style(glyph)),
        Span::raw(" "),
    ];
    spans.extend(lit_spans(&filter::row_text(ticket), lit, Style::new()));
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
    if let Branch::Root { rollup } = branch {
        spans.extend(rollup_spans("beneath", rollup));
    }
    let style = match ticket.status {
        Status::Done => Style::new().add_modifier(Modifier::DIM),
        _ => Style::new(),
    };
    Line::from(spans).style(style)
}

/// A context row on a sifted screen: the same row, dimmed whole. It is drawn to
/// say where the matches under it live — which map, and which takeable ticket
/// unlocks them — and nothing about it is actionable, so it carries no cursor
/// marker, no `also needs`, and no rollup. Nothing lit, either, and not merely
/// by omission: a row the query landed on is a match, and a match is drawn as
/// [`Item::Ticket`], never as one of these.
fn context_line(ticket: &Ticket, prefix: &str) -> Line<'static> {
    ticket_line(ticket, prefix, &[], &Branch::Plain, &[], false)
        .patch_style(Style::new().add_modifier(Modifier::DIM))
}

/// The body as styled lines: the [`Plan`] walked in order. Shared by the live
/// draw and the `TestBackend` tests.
pub fn body_lines(app: &App) -> Vec<Line<'static>> {
    body_with_cursor(app, &app.plan()).0
}

/// A collapsible group's line (#57): the cursor column, a `▸`/`▾` fold marker
/// where a ticket row's tree furniture would be, then the count it is holding.
/// It says `(hidden)` — and carries the stage rollup of what that is (#61) —
/// only while shut: once open, the rows are right there and claiming otherwise
/// would be a lie. A query opens the group onto its matches alone, and the
/// count says `n of m` for as long as that leaves anything out.
fn group_line(kind: GroupKind, held: usize, fold: &Fold, under_cursor: bool) -> Line<'static> {
    let marker = match fold {
        Fold::Open | Fold::Sifted { .. } => '▾',
        Fold::Shut { .. } => '▸',
    };
    // The glyph is never written here: each group stands for one row meaning, so
    // it names the [`RowGlyph`] and lets that type say which character it draws
    // (#78). Only the style is a local decision.
    let (glyph, label, style) = match kind {
        GroupKind::BlockedDeeper => (
            RowGlyph::Blocked,
            "blocked deeper down",
            glyph_style(RowGlyph::Blocked),
        ),
        // Deliberately not [`glyph_style`]'s DIM: that dims a *tally* of finished
        // rows, and this is a heading for a group you can open — the count beside
        // it is already dim, and dimming the glyph too would sink the line.
        GroupKind::Done => (
            RowGlyph::Stage(Stage::Done),
            "done",
            Style::new().fg(Color::Reset),
        ),
    };
    let count = match fold {
        Fold::Sifted { shown } if *shown < held => format!(" {shown} of {held} {label}"),
        _ => format!(" {held} {label}"),
    };
    let mut spans = vec![
        Span::raw("  "),
        cursor_span(under_cursor),
        Span::styled(format!("{marker} "), FURNITURE),
        Span::styled(glyph.char().to_string(), style),
        Span::styled(count, Style::new().add_modifier(Modifier::DIM)),
    ];
    if let Fold::Shut { rollup } = fold {
        spans.extend(rollup_spans("hidden", rollup));
    }
    Line::from(spans)
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
    let cursor_pos = app.cursor_pos();
    let mut stop = 0usize;
    let mut cursor_line = None;
    // Every stop the plan lists gets one line, in the same order, so this
    // single counter is what keeps the drawn ▶ and the cursor in agreement.
    // Which lines are stops is [`Item::stop_at`]'s call, not this loop's.
    let mut mark = |lines: &Vec<Line<'static>>, cursor_line: &mut Option<usize>| {
        let under_cursor = stop == cursor_pos;
        if under_cursor {
            *cursor_line = Some(lines.len());
        }
        stop += 1;
        under_cursor
    };

    // One query for the whole frame, not one per row: it carries the matcher's
    // scratch buffers. `None` on a structured screen, where there is nothing to
    // light up, so the rows below ask for a match only when a query is live.
    let mut query = filter::Query::new(&app.query);
    let mut lines = vec![Line::default()];
    for item in &plan.items {
        let under_cursor = item.stop_at().is_some() && mark(&lines, &mut cursor_line);
        match item {
            Item::Header(id) => {
                let map = &app.clusters[id];
                let lit = query.as_mut().map(|q| q.in_repo(map)).unwrap_or_default();
                lines.push(cluster_header(id, map, &lit));
            }
            Item::Ticket {
                row,
                prefix,
                also_needs,
                branch,
                depth: _,
            } => {
                let ticket = app.ticket(row);
                let lit = query
                    .as_mut()
                    .and_then(|q| q.hit(ticket))
                    .map(|hit| hit.in_row)
                    .unwrap_or_default();
                lines.push(ticket_line(
                    ticket,
                    prefix,
                    also_needs,
                    branch,
                    &lit,
                    under_cursor,
                ));
            }
            Item::Context { row, prefix } => lines.push(context_line(app.ticket(row), prefix)),
            Item::Group { id, held, fold } => {
                lines.push(group_line(id.kind, *held, fold, under_cursor));
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
///
/// # Panics
///
/// Never: the arm that takes the single failed id has already matched on
/// `len() == 1`.
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
///
/// # Panics
///
/// Never: as in [`heading`], the arm that takes the single failed id has
/// already matched on `len() == 1`.
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
fn draw_overlay(frame: &mut Frame<'_>, app: &App) {
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
    let key = launches.first().map(Launch::key).unwrap_or_default();
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
pub fn draw(frame: &mut Frame<'_>, app: &App) {
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

    // The count line's slot is shared: while a launch is staged (#62) the
    // launch line lives there instead — the resolved route, the ticket it
    // launches, and the mode text as it is typed.
    if let Overlay::LaunchLine { staged, text } = &app.overlay {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("  → {}", staged.route.label()),
                    Style::new().fg(Color::Cyan),
                ),
                Span::raw(format!(" · #{} {}  {text}", staged.ticket, staged.title)),
                Span::styled("█", Style::new().add_modifier(Modifier::DIM)),
            ])),
            count_area,
        );
    } else {
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
    }

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
    use crate::model::{classify, Activity, Map, MapId, MapSet, Ticket, TicketType};
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
            last_activity: None,
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

    /// Render the app through `TestBackend` and return the screen as text.
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
        // Glyph display order, the same one the rows and the rollups use:
        // the stage lattice, then the blocked override after it.
        assert!(
            screen.contains("▌ wayfinder · Map: wf  ○1  ◐1  ●1  ⊘2"),
            "{screen}"
        );
    }

    #[test]
    fn the_cluster_header_counts_stages_not_statuses() {
        // #6 is unblocked and unclaimed — `○` ready — until a PR with failing
        // checks makes it needs-attention. The row already drew that as `!`;
        // the header has to agree, because they are one vocabulary. Counting
        // statuses instead sweeps it back under `○`, and `◍`/`!` could never
        // appear in a header at all.
        let mut map = wf_map();
        map.tickets
            .iter_mut()
            .find(|t| t.number == 6)
            .expect("#6 in the fixture")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 46,
            status: PrStatus::Open {
                checks: Checks::Failing,
                review: Review::Required,
            },
        }];
        // #9 is claimed with a passing, approved PR: in review — the other
        // stage the four-status header had no glyph for.
        map.tickets
            .iter_mut()
            .find(|t| t.number == 9)
            .expect("#9 in the fixture")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 13,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }];
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let screen = render(&App::new(clusters));
        // ○0 is not drawn: the counts name the stages the map is actually in.
        assert!(
            screen.contains("▌ wayfinder · Map: wf  ◍1  !1  ●1  ⊘2"),
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
        // Each root closes with the stage rollup of the branch beneath it, in
        // the same glyph vocabulary as the header above and the rows below:
        // #6 unlocks #7 and #14, #9 unlocks #14 alone.
        assert!(
            screen.contains("#6 Re-entry breadcrumbs [task] (beneath) ⊘2"),
            "{screen}"
        );
        assert!(
            screen.contains("#9 Main screen design [task] (beneath) ⊘1"),
            "{screen}"
        );
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
    fn stage_glyphs_follow_the_prs_on_the_row() {
        // The status column *is* the stage column now (#61/#62): an approved
        // open PR reads ◍ in review, failing checks read ! needs attention —
        // whatever the ticket's own state says.
        let mut map = wf_map();
        map.tickets
            .iter_mut()
            .find(|t| t.number == 6)
            .expect("#6")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }];
        map.tickets
            .iter_mut()
            .find(|t| t.number == 9)
            .expect("#9")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 91,
            status: PrStatus::Open {
                checks: Checks::Failing,
                review: Review::NotRequired,
            },
        }];
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let screen = render(&App::new(clusters));
        assert!(screen.contains("◍ #6 Re-entry breadcrumbs"), "{screen}");
        assert!(screen.contains("! #9 Main screen design"), "{screen}");
        // Blocked still overrides whatever stage lies beneath.
        assert!(screen.contains("⊘ #7 Supervising AFK agents"), "{screen}");
    }

    #[test]
    fn a_shut_group_line_carries_its_stage_rollup() {
        // Done ticket #2's PR is still open with pending checks: the shut
        // group says so as a glyph+count pair, so a closed-but-unlanded branch
        // is watched at a glance without opening it.
        let mut map = wf_map();
        map.tickets
            .iter_mut()
            .find(|t| t.number == 2)
            .expect("#2")
            .prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 92,
            status: PrStatus::Open {
                checks: Checks::Pending,
                review: Review::Required,
            },
        }];
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let mut app = App::new(clusters);
        let screen = render(&app);
        assert!(screen.contains("● 1 done (hidden) ◐1"), "{screen}");

        // Open, the rollup leaves with "(hidden)": the row is right there.
        while !matches!(app.cursor_stop(), Some(crate::view::Stop::Group(_))) {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let screen = render(&app);
        let done_line = screen
            .lines()
            .find(|l| l.contains("done"))
            .expect("the group line");
        assert!(!done_line.contains("◐1"), "{screen}");
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
                last_activity: None,
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
                last_activity: None,
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
        assert!(
            first < second,
            "equal activity falls back to (repo, number)"
        );
        assert!(screen.contains("#50 Build: clusters"), "{screen}");
        // One repo on screen, however many maps: the title names the repo.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
    }

    #[test]
    fn a_finished_map_renders_below_the_live_ones_however_recent_it_is() {
        // The reported symptom: the finished map held the lowest issue number
        // *and* the freshest activity, and so sat at the top of the tree. Both
        // of the keys that would have put it there are deliberately set here.
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/archive", 1),
            Map {
                title: "Map: archive".to_string(),
                last_activity: Activity::parse("2026-08-06T12:00:00Z"),
                tickets: vec![ticket(
                    "blooop/archive",
                    88,
                    "Shipped last week",
                    false,
                    false,
                    vec![],
                )],
            },
        );
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: the selection view".to_string(),
                last_activity: Activity::parse("2026-08-01T12:00:00Z"),
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
        // The forest, because that is the screen a finished map appears on at
        // all — leverage drops it as idle.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        let live = screen
            .find("▌ wayfinder · Map: the selection view")
            .expect("the live cluster");
        let finished = screen
            .find("▌ archive · Map: archive")
            .expect("the finished cluster");
        assert!(
            live < finished,
            "finished maps sink to the bottom:\n{screen}"
        );
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
    fn a_query_sifts_the_tree_rather_than_flattening_it() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        let screen = render(&app);
        // The cluster keeps its header, and each match keeps the takeable row
        // it hangs from — #14 needs both #6 and #9, so it is drawn under both,
        // exactly as the unsifted leverage screen draws it.
        assert!(screen.contains("▌ wayfinder · Map: wf"), "{screen}");
        let body: Vec<&str> = screen
            .lines()
            .filter(|l| l.contains('#'))
            .map(|l| l.trim_matches('│').trim_end())
            .collect();
        assert_eq!(
            body,
            vec![
                "  ▶ ○ #6 Re-entry breadcrumbs [task]",
                "    └─  ⊘ #14 Breadcrumb markers [task]",
                "    ◐ #9 Main screen design [task]",
                "    └─  ⊘ #14 Breadcrumb markers [task]",
            ],
            "{screen}"
        );
        // #9 is on screen only to place the match beneath it — the cursor
        // cannot land on it, and it is not counted as a hit.
        assert_eq!(screen.matches('▶').count(), 1, "{screen}");
        // Rows the query dropped are gone, group and all.
        assert!(!screen.contains("Choose the stack"), "{screen}");
        assert!(!screen.contains("Supervising AFK agents"), "{screen}");
        assert!(!screen.contains("done"), "{screen}");
        // Count line and prompt reflect the live query; the denominator is
        // the map's tickets, not the leverage rows.
        assert!(screen.contains("3/5"), "{screen}");
        assert!(screen.contains("> bread█"), "{screen}");
        // Clearing the query restores the lens whole.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("Supervising AFK agents"), "{screen}");
        assert!(screen.contains("1 done"), "{screen}");
    }

    /// The body with the characters a live query lit wrapped in `«»` — the
    /// underlining, in a form a test can read.
    fn lit_body(app: &App) -> Vec<String> {
        body_lines(app)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| {
                        if span.style.add_modifier.contains(MATCHED) {
                            format!("«{}»", span.content)
                        } else {
                            span.content.to_string()
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_live_query_underlines_the_characters_it_matched() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        assert_eq!(
            lit_body(&app)
                .into_iter()
                .filter(|l| l.contains('#'))
                .collect::<Vec<_>>(),
            vec![
                "  ▶ ○ #6 Re-entry «bread»crumbs [task]",
                "    └─  ⊘ #14 «Bread»crumb markers [task]",
                // The context row is why the match above it is on screen, not
                // a match itself: nothing on it is lit.
                "    ◐ #9 Main screen design [task]",
                "    └─  ⊘ #14 «Bread»crumb markers [task]",
            ],
        );
    }

    #[test]
    fn a_query_that_matched_the_repo_underlines_it_in_the_cluster_header() {
        // Typing a project name sifts the screen down to that project while
        // landing on no character any row draws. Without the header answering
        // for it, the screen would show a wall of matches with no match in it.
        let mut app = fixture_app();
        type_str(&mut app, "wayf");
        let body = lit_body(&app);
        assert!(
            body.iter()
                .any(|l| l.starts_with("▌ «wayf»inder · Map: wf")),
            "{body:?}"
        );
        assert!(
            body.iter().all(|l| !l.contains('#') || !l.contains('«')),
            "the match is on the repo, so no row claims it: {body:?}"
        );
    }

    #[test]
    fn a_sifted_group_says_how_many_of_how_many_it_is_showing() {
        // Two done tickets, one of them matching: typing has to reach inside
        // the group that holds it, and the group has to stay honest about the
        // one it is still holding back.
        let mut map = wf_map();
        map.tickets.push(ticket(
            "blooop/wayfinder",
            3,
            "Stack the PRs",
            false,
            false,
            vec![],
        ));
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let mut app = App::new(clusters);
        type_str(&mut app, "choose");
        let screen = render(&app);
        assert!(screen.contains("▾ ● 1 of 2 done"), "{screen}");
        assert!(screen.contains("● #2 Choose the stack"), "{screen}");
        assert!(!screen.contains("Stack the PRs"), "{screen}");
        // The cursor is on the match, not on the group heading above it —
        // there is no fold to toggle while a query is what opened it.
        assert_eq!(app.cursor_ticket().map(|t| t.number), Some(2));
        assert_eq!(screen.matches('▶').count(), 1, "{screen}");
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
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // stage
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // resolve
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
    fn the_launch_line_replaces_the_count_line_and_shows_the_route() {
        let mut app = fixture_app();
        let screen = render(&app);
        assert!(screen.contains("5/5"), "{screen}");

        // Enter on #6 (a task at ready): the line opens where the count was,
        // naming the resolved route and the ticket it launches.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(
            screen.contains("→ /wayfinder · #6 Re-entry breadcrumbs"),
            "{screen}"
        );
        assert!(
            !screen.contains("5/5"),
            "the count line is replaced: {screen}"
        );

        // The typed mode shows on the line as it accumulates.
        type_str(&mut app, "defer something");
        let screen = render(&app);
        assert!(screen.contains("defer something"), "{screen}");

        // Esc gives the count line back.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("5/5"), "{screen}");
        assert!(!screen.contains("→ /wayfinder"), "{screen}");
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
                last_activity: None,
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
                last_activity: None,
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
