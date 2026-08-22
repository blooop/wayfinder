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

use crate::app::{App, Overlay};
use crate::filter;
use crate::launch::{Agent, Candidate, Launch, Staged};
use crate::liveness::Life;
use crate::model::{Checks, Map, MapId, PrLink, PrStatus, Review, RowGlyph, Stage, Status, Ticket};
use crate::reclaim::Reclaimable;
use crate::view::{Branch, Fold, GroupKind, Item, Plan, ProjectRow};

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
fn cluster_header(
    id: &MapId,
    map: &Map,
    lit: &[usize],
    under_cursor: bool,
    marks: Marks,
) -> Line<'static> {
    let cyan = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    // The cursor rides *before* the `▌`, not after it: the bar is the cluster's
    // left edge and every row below hangs off it, so a marker inside it would
    // read as belonging to the first row rather than to the header (#96).
    let mut spans = vec![cursor_span(under_cursor), Span::styled("▌ ", cyan)];
    spans.extend(lit_spans(id.short_repo(), lit, cyan));
    spans.push(Span::styled(format!(" · {}", map.title), cyan));
    // A map is a node, so a charting session is as resumable as a build one,
    // and a map's own workspace is as launchable — the header is the only place
    // the list can say either (#35).
    spans.extend(marks.spans());
    for (glyph, count) in map.tally() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{}{count}", glyph.char()),
            glyph_style(glyph),
        ));
    }
    Line::from(spans)
}

/// A project row: the body of the project list, and the line a project's own
/// screen opens on.
///
/// The same left edge every cluster has, one shade quieter — a project is the
/// container its maps sit in, so it reads as somewhere to stand rather than as
/// something that has happened. It carries the full `owner/name` rather than
/// the short name the cluster headers show, because this is the one place two
/// forks of one repo sit next to each other and the short names would be
/// identical.
///
/// The tail says what is inside, and has three honest answers: the stage
/// rollup once maps are on hand, `no map — enter to start one` once the search
/// has answered and found none, and `loading…` in between. That third one is
/// the whole reason [`ProjectRow::loaded`] exists — a repo whose maps are still
/// in flight and a repo that has none are the same zero, and calling the first
/// "no map" is the lie [`crate::refresh::Startup`] exists to prevent.
fn project_line(row: &ProjectRow, under_cursor: bool) -> Line<'static> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let cyan = Style::new().fg(Color::Cyan);
    let mut spans = vec![cursor_span(under_cursor), Span::styled("▌ ", cyan)];
    spans.extend(lit_spans(&row.repo, &row.lit, cyan));
    match (row.maps, row.loaded) {
        (0, false) => spans.push(Span::styled(" · loading…", dim)),
        (0, true) => spans.push(Span::styled(" · no map — enter to start one", dim)),
        (n, _) => {
            spans.push(Span::styled(
                if n == 1 {
                    " · 1 map".to_string()
                } else {
                    format!(" · {n} maps")
                },
                dim,
            ));
            for &(glyph, count) in &row.rollup {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("{}{count}", glyph.char()),
                    glyph_style(glyph),
                ));
            }
        }
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
///
/// `resumable` adds the `⏎` badge (#35): a conversation from a previous launch
/// is waiting on this node. A bare flag rather than the record itself, because
/// the row says only *that* there is a way back — how old it is belongs to the
/// picker, where the choice is actually made.
fn ticket_line(
    ticket: &Ticket,
    prefix: &str,
    also_needs: &[u64],
    branch: &Branch,
    lit: &[usize],
    under_cursor: bool,
    marks: Marks,
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
    spans.extend(marks.spans());
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
    // No resume badge either, for the same reason it carries no cursor: a
    // context row is not somewhere `enter` can be pressed, so a badge about
    // what `enter` would do there would be an offer the row cannot honour.
    //
    // No liveness marking, on the narrower ground these rows already stand on:
    // a context row shows no PR badges either. It is drawn to say *where* the
    // matches beneath it live, and everything it says about itself is furniture
    // — so a stalled node reachable only as somebody else's context is not
    // marked on a sifted screen, and is there the moment the query clears.
    ticket_line(
        ticket,
        prefix,
        &[],
        &Branch::Plain,
        &[],
        false,
        Marks::default(),
    )
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
                lines.push(cluster_header(
                    id,
                    map,
                    &lit,
                    under_cursor,
                    Marks::of(app, &id.repo, id.number),
                ));
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
                    Marks::of(app, &ticket.repo, ticket.number),
                ));
            }
            Item::Project(project) => lines.push(project_line(project, under_cursor)),
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
/// The hint line, which is the level's — because the keys mean different
/// enough things on the two screens that one line would have to be vague about
/// both. `enter` opens a project up here and launches an agent down there;
/// `tab` and the depth keys have nothing to toggle or descend into on a flat
/// list of projects; and `←` being the way *back* is worth saying exactly where
/// there is somewhere to go back to.
fn key_hints(app: &App) -> &'static str {
    if app.current_repo().is_some() {
        "  enter launch · ←→ open · ← back · tab structure · esc quit"
    } else {
        "  enter open · ↑↓ move · type to filter · esc quit"
    }
}

/// The project heading in the title bar.
///
/// On a project's own screen it is that project — the level *is* the screen, so
/// the title says which one whatever the fetch has managed so far.
///
/// On the project list it counts the registered repos, which is a fact off the
/// cache and needs no network, so the ambiguity the old heading had to
/// disentangle is gone with the screen that had it: an empty list now means one
/// thing (nothing registered) instead of three (still loading, every fetch
/// failed, or genuinely none). A failed *map* fetch no longer empties this
/// screen at all — the projects are still listed — so it is named here only
/// while it is the most interesting thing true, and on the count line
/// ([`failure_note`]) in every case.
///
/// # Panics
///
/// Never: the arm that takes the single failed id has already matched on
/// `len() == 1`.
pub fn heading(app: &App) -> String {
    if let Some(repo) = app.current_repo() {
        return repo.to_string();
    }
    let projects = app.projects();
    if projects.is_empty() {
        return "no projects — run wf inside a checkout to register it".to_string();
    }
    // Naming the map is the whole value when there is one: "GitHub is
    // unreachable" and "that project has nothing open" are different problems
    // with different fixes, and a bare count reads the same either way.
    //
    // This named the refresh chord as the retry until that key was retired.
    // Naming a key that no longer exists is worse than saying nothing, and
    // saying nothing leaves the reader on a screen with a failure and no move
    // to make — so it names the move that is left. Restarting really is the
    // retry now: each map is fetched once per run, and a warm start is ~0.6 s.
    if app.clusters.is_empty() && app.startup.is_loaded() {
        match app.failed.len() {
            0 => {}
            1 => {
                let id = app.failed.iter().next().expect("len checked");
                return format!("{}#{} — fetch failed, run wf again", id.repo, id.number);
            }
            n => return format!("{n} maps failed to fetch — run wf again"),
        }
    }
    match projects.len() {
        1 => projects.into_iter().next().expect("len checked"),
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

/// The `· … truncated` segment on the count line (#184): a map the tracker
/// could not send all of — more sub-issues or blocking edges than one page
/// holds, per [`Map`]'s `truncated` — named where the reader is, because the
/// body draws a normal-looking tree either way and this line is the only
/// place left to say the tree is partial.
///
/// Shaped exactly like [`failure_note`], its nearest kin: both are persistent
/// facts about a whole map that the rows cannot carry, one map is named, more
/// collapse to a count.
pub fn truncated_note(app: &App) -> String {
    let mut truncated = app.clusters.iter().filter(|(_, map)| map.truncated);
    match (truncated.next(), truncated.count()) {
        (None, _) => String::new(),
        (Some((id, _)), 0) => format!("· {}#{} truncated", id.repo, id.number),
        (Some(_), more) => format!("· {} maps truncated", more + 1),
    }
}

/// The `· N reclaimable: …` segment on the count line (#137): what a
/// `wf reap` would claim, once the background reading has landed.
///
/// Empty while the reading is out, and empty forever if it failed or found
/// nothing — the three are one silence on purpose, because none of them is
/// something a person picking a ticket needs told about.
///
/// It names the workspaces rather than only counting them: a bare number is
/// something a reader has no way to agree or disagree with, and the point of
/// surfacing this at all is that `wf reap` is then a decision rather than an
/// errand. The picker does nothing with it — the segment *is* the feature.
///
/// `width` is what is left of the count line once everything else on it has
/// been laid down. Real workspace ids are ~40 characters, so this segment is
/// the one on the line that has to be *sized* rather than written and clipped:
/// see [`Reclaimable::hint`] for what it gives up first, and what it never
/// gives up.
pub fn reclaim_note(app: &App, width: usize) -> String {
    app.reclaimable
        .as_ref()
        .map(|found| Reclaimable::hint(found, width))
        .unwrap_or_default()
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

/// The badge a row carries when a previous launch left a conversation on it
/// (#35) — the same glyph as the key that rejoins it.
const RESUME_BADGE: &str = "  ⏎";

/// The badge a node carries when a container of its is up.
///
/// A square, where every stage glyph is round, because it is not a stage: the
/// row's leading glyph is what the *tracker* says the work is, and this is what
/// the *machine* says is happening to it. Two vocabularies that must not be
/// mistaken for one another at a glance, so they do not share a shape.
const RUNNING_BADGE: &str = "  ▣";

/// The badge a node carries when it is claimed, has nothing pushed, and nothing
/// of its is running.
///
/// It sits next to a `◐ building` stage glyph, and that juxtaposition *is* the
/// finding: the tracker believes this is in progress, and no container of its
/// is up. An hourglass because what it reports is elapsed time and nothing else
/// — see [`Life::Stalled`](crate::liveness::Life::Stalled) for how little that
/// claims.
const STALLED_BADGE: &str = "  ⧖";

/// What the app knows about a row's node that the ticket itself cannot say.
///
/// Both fields are per-node lookups against state the tracker never supplied —
/// one from the launch record, one from this machine — and both are decided at
/// the same call site for the same `(repo, number)`. Carrying them as one value
/// is what keeps a row's signature from growing a parameter every time `wf`
/// learns something new about a node, and it puts the two badges in one place
/// so a ticket row and a cluster header cannot drift apart about their order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Marks {
    /// A previous launch left a conversation on this node (#35).
    resumable: bool,
    /// What this machine says is happening to it, if anything.
    life: Option<Life>,
}

impl Marks {
    /// Everything the app has to say about one node.
    fn of(app: &App, repo: &str, number: u64) -> Self {
        Self {
            resumable: app.resume(repo, number).is_some(),
            life: app.liveness.of(repo, number),
        }
    }

    /// The badges, in the one order both row kinds draw them: what `enter`
    /// would rejoin, then what is happening to it now.
    fn spans(self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if self.resumable {
            spans.push(Span::styled(
                RESUME_BADGE,
                Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM),
            ));
        }
        spans.extend(life_span(self.life));
        spans
    }
}

/// The spans a row's liveness marking contributes, if any.
///
/// **No new colour.** Cyan is already "`wf`'s own record about this row" — it
/// is what `⏎` is drawn in — and these two are the same kind of fact from the
/// same background reading, so they join it rather than minting a seventh hue
/// on a screen that deliberately spends six (see [`CURSOR_COLOR`], and the
/// query-match modifier that was chosen over a colour for the same reason).
///
/// The two are told apart by weight instead: a running container is live, so it
/// is undimmed; a stall is a session that is over, which is exactly what DIM
/// means everywhere else here. That leaves the badge for the thing that has
/// stopped quieter than the one still going, which is the right way round for a
/// picker — the loud version of "stalled" belongs on the count line, where it
/// is a summons and not a decoration.
fn life_span(life: Option<Life>) -> Option<Span<'static>> {
    let cyan = Style::new().fg(Color::Cyan);
    match life? {
        Life::Running => Some(Span::styled(RUNNING_BADGE, cyan)),
        Life::Stalled => Some(Span::styled(
            STALLED_BADGE,
            cyan.add_modifier(Modifier::DIM),
        )),
    }
}

/// How long ago `then` was, from `now`, in the one unit that reads: `20m ago`,
/// `3h ago`, `5d ago`.
///
/// A pure function of the two instants rather than a method reading the clock,
/// so the rendering is pinned by tests instead of raced against a wall clock —
/// the same reason the launch context carries no snapshot instant.
///
/// A clock that moved backwards between the launch and now reads as **the
/// present**. Both alternatives are worse: a negative age cannot be spelt, and
/// an unsigned subtraction would wrap to an age of a hundred billion years.
fn ago(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// Now, in seconds since the Unix epoch — read once per frame that draws an
/// age, and never anywhere a test asserts on.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

/// The launch picker: what `enter` on a node opens, and everything that launch
/// still needs decided.
///
/// A modal rather than #62's one-line prompt, because the line could only *echo*
/// what you typed — the modes were words you had to already know (`defer`, then
/// `auto`), and an unattended launch looked exactly like a typo until it ran. The
/// options are rows now: each one names its mode, the skill that mode routes the
/// staged node to, and who ends up deciding. Which is also why the route is
/// drawn per option rather than once for the cursor's — the difference between
/// `/wf`, `/wf-mid`, `/wf-auto` and no skill at all (#112) *is* the choice
/// being made, so every one of them is on screen.
/// The agent is in the title rather than another row: it applies to every
/// candidate, and horizontal keys can change it without moving the vertical
/// cursor.
fn draw_launch_picker(
    frame: &mut Frame<'_>,
    staged: &Staged,
    agent: Agent,
    candidate: Candidate,
    steer: &str,
) {
    let mut lines = vec![Line::default()];
    for option in staged.candidates() {
        let picked = option == candidate;
        let marker = if picked { '▶' } else { ' ' };
        // The cursor's row is the one that will run, so it is the one that reads
        // as bold; the others stay dim enough to scan past.
        let emphasis = if picked {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        // A resume names when the conversation was left, which its label
        // cannot: three weeks old and twenty minutes old are the same row
        // otherwise, and they are not the same decision.
        let blurb = match (option, staged.resume()) {
            (Candidate::Resume { .. }, Some(resume)) => {
                format!("{} · {}", ago(resume.at, now_secs()), option.blurb())
            }
            _ => option.blurb().to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {marker} {:<14}", option.label()), emphasis),
            Span::styled(
                format!("{:<18}", option.invocation(agent)),
                Style::new().fg(Color::Cyan),
            ),
            Span::styled(blurb, Style::new().add_modifier(Modifier::DIM)),
        ]));
    }
    lines.push(Line::default());
    // The text field is always shown, empty or not: it is the other half of
    // what enter will do, and a field that appears only once you have typed
    // into it cannot tell you that you may. Its *name* is the picked row's
    // (#114) — one field, three meanings, and the row says which one is live:
    // steering an agent, the task itself, or a seed for a charting session.
    lines.push(Line::from(vec![
        Span::styled(
            format!("    {:<6} ", candidate.field()),
            Style::new().add_modifier(Modifier::DIM),
        ),
        Span::raw(steer.to_string()),
        Span::styled("█", Style::new().add_modifier(Modifier::DIM)),
    ]));
    lines.push(Line::default());
    lines.push(Line::styled(
        "  enter launch · ←/→ agent · ↑/↓ pick · type to fill · esc cancel",
        Style::new().add_modifier(Modifier::DIM),
    ));
    // The repo leads the title because it is the one fact *every* row shares
    // (#114): the launch rows work this node, and the creation rows start
    // something new in the repo it belongs to. Naming only the node would be
    // true of half the list — and the repo is exactly what creation would
    // otherwise have to ask for.
    // The picked row decides which agent the title names: on a resume it is
    // the recorded one, since that is what `enter` will become. Anything else
    // would put a Codex title over a row that runs Claude.
    let title = format!(
        " launch {} · {} · {} {} ",
        candidate.agent(agent).label(),
        staged.repo,
        staged.key(),
        staged.title()
    );
    let title = title.trim_end().to_string() + " ";
    let width = lines
        .iter()
        .map(|l| l.width() as u16 + 4)
        .chain(std::iter::once(title.chars().count() as u16 + 4))
        .max()
        .unwrap_or(40);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

/// Whichever modal is up, over the screen the rest of [`draw`] already drew.
/// Exhaustive on the overlay, so a third modal cannot be added without being
/// given something to draw.
fn draw_overlay(frame: &mut Frame<'_>, app: &App) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::PickLaunch {
            staged,
            candidate,
            agent,
            steer,
        } => draw_launch_picker(frame, staged, *agent, *candidate, steer),
        Overlay::PickCheckout { launches, cursor } => {
            draw_checkout_picker(frame, launches, *cursor);
        }
    }
}

/// The which-checkout modal: one row per registered checkout of the repo.
///
/// The one prompt `wf` still has, and the reason it survived the Build 7
/// deletion (#34): a repo can have several checkouts, the agent must run in
/// exactly one, and `wf` cannot guess which. The path *is* the row — it is what
/// distinguishes the candidates, and with no session to name there is nothing
/// shorter to show alongside it.
fn draw_checkout_picker(frame: &mut Frame<'_>, launches: &[Launch], cursor: usize) {
    let mut lines = vec![Line::default()];
    for (i, launch) in launches.iter().enumerate() {
        let marker = if i == cursor { '▶' } else { ' ' };
        // Two trees of one repo can differ in what the agent will run in —
        // one carrying a devcontainer and one not — so the choice has to say
        // so here, where it is being made, not only in the notice after (#80).
        lines.push(Line::from(vec![
            Span::raw(format!("  {marker} ")),
            Span::styled(
                launch.cwd().display().to_string(),
                Style::new().fg(Color::Cyan),
            ),
            Span::styled(
                launch.isolation().suffix(),
                Style::new().add_modifier(Modifier::DIM),
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

/// Draw the full screen: bordered frame with the scope in the title, then the
/// search chrome anchored at the *top* — the fzf-style prompt and, under it, the
/// match-count line (with the load hint and any one-shot notice) — the clusters
/// filling what is left, and the key hints on the last line. The prompt leads
/// because it is what you type into: `fzf --reverse`'s order, and it keeps the
/// count line against the query whose matches it counts.
/// The which-checkout picker, when open, floats over all of it.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let mut block = Block::bordered().title(format!(" wf · {} ", heading(app)));
    // The way back, named on the screen it applies to. A project screen is the
    // only one with anywhere to go back to, and `←` is the only key that goes
    // there — so this line appears exactly when it is true.
    if app.current_repo().is_some() {
        block = block.title_top(Line::from(" ← all projects ").right_aligned());
    }
    let inner = block.inner(frame.area());
    frame.render_widget(block, frame.area());

    let [prompt_area, count_area, body_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  > ", Style::new().fg(Color::Cyan)),
            Span::raw(app.query.clone()),
            Span::styled("█", Style::new().add_modifier(Modifier::DIM)),
        ])),
        prompt_area,
    );

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
    // rather than leaving gaps. The count line keeps its slot while a launch is
    // staged — the staged launch is a modal now (#62's line became
    // [`draw_launch_picker`]), and a modal does not need the row it covers to
    // move out of its way.
    let mut parts: Vec<String> = [
        app.startup.hint(),
        failure_note(app),
        truncated_note(app),
        idle_note(&plan),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect();
    let (shown, total) = app.counts();
    let counts = format!("  {shown}/{total}");
    let notice = app
        .notice
        .as_ref()
        .map(|notice| format!("   {notice}"))
        .unwrap_or_default();
    // The reclaim segment is sized rather than written and clipped, because it
    // is the only one on this line whose content is unbounded — a `dl`
    // workspace id is ~40 characters and there can be three of them. It goes
    // last and takes what the rest of the line has left, which is why the rest
    // of the line is measured first.
    let spent = counts.chars().count()
        + 2
        + parts
            .iter()
            .map(|part| part.chars().count() + 1)
            .sum::<usize>()
        + notice.chars().count();
    let mut left = (count_area.width as usize).saturating_sub(spent);
    // Stalls are laid down before the reclaim note, but not at any price: what
    // is held back for the reclaim note is exactly the width at which it stops
    // being a word (`Reclaimable::min_width`), because its own last arm clips
    // the count rather than vanishing. Without that, two named stalls on a
    // 60-column terminal left it three characters short and it rendered
    // `· 2 reclaima`.
    //
    // Yielding is the whole of the concession. Stalls still outrank the reap
    // pointer and the warned aside — both of those go while `· N stalled` is
    // still naming nodes — and `Liveness::hint`'s own ladder does the yielding
    // gracefully, dropping to one name and then to the bare count, which the
    // rows' own `⧖` markings make readable anyway.
    let reserved = app
        .reclaimable
        .as_ref()
        .map_or(0, |found| found.min_width() + 1);
    let stalled = app.liveness.hint(left.saturating_sub(reserved));
    if !stalled.is_empty() {
        left = left.saturating_sub(stalled.chars().count() + 1);
        parts.push(stalled);
    }
    let note = reclaim_note(app, left);
    if !note.is_empty() {
        parts.push(note);
    }
    let status = parts.join(" ");
    let mut count_spans = vec![
        Span::raw(counts),
        Span::styled(
            format!("  {status}"),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ];
    if !notice.is_empty() {
        count_spans.push(Span::styled(
            notice,
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(count_spans)), count_area);

    frame.render_widget(
        Paragraph::new(key_hints(app)).style(Style::new().add_modifier(Modifier::DIM)),
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
            truncated: false,
            tickets: vec![
                t(2, "Choose the stack", false, true, vec![]),
                t(6, "Re-entry breadcrumbs", true, false, vec![]),
                t(7, "Supervising AFK agents", true, false, vec![6]),
                t(9, "Main screen design", true, true, vec![]),
                t(14, "Breadcrumb markers", true, false, vec![6, 9]),
            ],
        }
    }

    /// An app standing on `repo`'s screen. `App::new` opens on the project
    /// *list*, which draws no cluster at all, so a test about what a cluster
    /// looks like has to say which project's screen it is on.
    fn app_on(repo: &str, clusters: BTreeMap<MapId, Map>) -> App {
        let mut app = App::new(clusters);
        app.enter(repo);
        app
    }

    fn fixture_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        app_on("blooop/wayfinder", clusters)
    }

    /// Step the cursor down `n` stops.
    fn down(app: &mut App, n: usize) {
        for _ in 0..n {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Render the app through `TestBackend` and return the screen as text.
    fn render(app: &App) -> String {
        render_at(90, app)
    }

    /// The same, on a terminal of a stated width — for the claims that are
    /// about what survives a narrow one.
    fn render_at(width: u16, app: &App) -> String {
        let backend = TestBackend::new(width, 24);
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

    /// The fixture app, with a conversation left on `number`.
    fn app_resuming(number: u64) -> App {
        fixture_app().with_sessions(vec![crate::projects::Session::new(
            "blooop/wayfinder".to_string(),
            number,
            Agent::Claude,
            std::path::PathBuf::from("/data/proj/wayfinder"),
            crate::launch::Isolation::Host,
        )])
    }

    #[test]
    fn a_row_you_left_a_conversation_on_says_so_before_anything_is_fetched() {
        // The badge is drawn from the local cache, so it is on the *first*
        // frame — the same frame-zero rule the project list follows. Nothing
        // asks `dl`, and nothing waits for the map search.
        let screen = render(&app_resuming(7));
        let row = screen
            .lines()
            .find(|l| l.contains("#7 Supervising"))
            .expect("the row is on screen");
        assert!(row.contains('⏎'), "{row}");
        // And only that row: the badge is a fact about one node.
        let other = screen
            .lines()
            .find(|l| l.contains("#6 Re-entry"))
            .expect("the neighbour is on screen");
        assert!(!other.contains('⏎'), "{other}");
    }

    #[test]
    fn a_map_you_have_charted_before_carries_the_badge_too() {
        // A map is a node (#96) and its picker offers the resume row, so the
        // list has to say so — otherwise the only way to discover a charting
        // session is waiting is to press enter on every header.
        let screen = render(&app_resuming(1));
        let header = screen
            .lines()
            .find(|l| l.contains("Map: wf"))
            .expect("the cluster header is on screen");
        assert!(header.contains('⏎'), "{header}");
    }

    #[test]
    fn the_resume_row_says_how_long_ago_you_left_it() {
        // Whether a conversation is twenty minutes or three weeks old changes
        // whether you want it back, and it is the one thing the row's label
        // cannot say. The rendering is a pure function of the two instants, so
        // it is pinned here rather than against a wall clock.
        assert_eq!(ago(1_000, 1_000), "just now");
        assert_eq!(ago(1_000, 1_030), "just now");
        assert_eq!(ago(1_000, 1_000 + 20 * 60), "20m ago");
        assert_eq!(ago(1_000, 1_000 + 3 * 3_600), "3h ago");
        assert_eq!(ago(1_000, 1_000 + 5 * 86_400), "5d ago");
        // A clock that went backwards between the launch and now reads as the
        // present rather than as a negative age or a huge one.
        assert_eq!(ago(2_000, 1_000), "just now");
    }

    #[test]
    fn the_picker_draws_the_way_back_as_the_argv_it_will_actually_run() {
        // The skill column names what execs. Every other row names a skill;
        // this one has none, so it names the agent's own resume flags — which
        // is exactly the difference between resuming and `plain`, and the two
        // sit one row apart.
        let mut app = app_resuming(6);
        down(&mut app, 2);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("resume"), "{screen}");
        assert!(screen.contains("claude --continue"), "{screen}");
    }

    #[test]
    fn the_picker_title_names_the_agent_that_will_actually_rejoin() {
        // The title is the agent axis's readout, and on the resume row the
        // axis is the record's rather than the picker's. A Codex title over a
        // row that runs Claude is the one lie this screen could tell.
        let mut app = fixture_app().with_sessions(vec![crate::projects::Session::new(
            "blooop/wayfinder".to_string(),
            6,
            Agent::Codex,
            std::path::PathBuf::from("/data/proj/wayfinder"),
            crate::launch::Isolation::Host,
        )]);
        down(&mut app, 2);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("launch Codex"), "{screen}");
        assert!(screen.contains("codex resume --last"), "{screen}");
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
        let screen = render(&app_on("blooop/wayfinder", clusters));
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
        let app = app_on("blooop/wayfinder", clusters);
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
        let screen = render(&app_on("blooop/wayfinder", clusters));
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
        let mut app = app_on("blooop/wayfinder", clusters);
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
        let screen = render(&app_on("blooop/wayfinder", clusters));
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

    /// The two markings land on the rows they are about, and say different
    /// things: one node has a container up, another is claimed with nothing
    /// running. Both come from the same reading.
    #[test]
    fn a_running_container_and_a_stall_are_marked_on_the_rows_they_belong_to() {
        let mut app = fixture_app();
        app.liveness = crate::liveness::Liveness::for_test(
            &[("blooop/wayfinder", 6)],
            &[("blooop/wayfinder", 9)],
        );
        let screen = render(&app);
        let row = |needle: &str| {
            screen
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no row for {needle}: {screen}"))
                .to_string()
        };
        assert!(
            row("Re-entry breadcrumbs").contains('▣'),
            "a node with a container up says so: {screen}"
        );
        assert!(
            row("Main screen design").contains('⧖'),
            "a claimed node with nothing running says so: {screen}"
        );
        // The two are never the same mark, and neither leaks onto a row the
        // reading said nothing about.
        assert!(!row("Re-entry breadcrumbs").contains('⧖'), "{screen}");
        assert!(!row("Supervising AFK agents").contains('▣'), "{screen}");
        assert!(!row("Supervising AFK agents").contains('⧖'), "{screen}");
    }

    #[test]
    fn the_first_frame_marks_no_row_and_names_no_stall() {
        // No reading has landed, which is the state every run starts in and
        // stays in when `dl` is absent or the listing failed.
        let app = fixture_app();
        let screen = render(&app);
        assert!(!screen.contains('▣'), "{screen}");
        assert!(!screen.contains('⧖'), "{screen}");
        assert!(!screen.contains("stalled"), "{screen}");
    }

    /// A stall reaches the count line as well as the row, because the row is
    /// not always on screen: the project list has no ticket rows at all, and a
    /// stalled node can be inside a fold or another map.
    #[test]
    fn the_count_line_names_what_stopped_moving() {
        let mut app = fixture_app();
        app.liveness = crate::liveness::Liveness::for_test(
            &[],
            &[("blooop/wayfinder", 9), ("blooop/wayfinder", 14)],
        );
        let screen = render(&app);
        let line = screen
            .lines()
            .find(|line| line.contains("stalled"))
            .unwrap_or_else(|| panic!("no stall segment: {screen}"));
        assert!(line.contains("2 stalled"), "{line}");
        assert!(line.contains("wayfinder#9"), "{line}");
    }

    /// The two variable-length segments share one line, and neither may leave
    /// the other unreadable: the `wf reap` pointer is the half of the reclaim
    /// hint a reader can act on, and it still fits.
    #[test]
    fn a_stall_segment_leaves_the_reclaim_pointer_its_room() {
        let mut app = fixture_app();
        app.liveness = crate::liveness::Liveness::for_test(
            &[],
            &[
                ("blooop/wayfinder", 9),
                ("blooop/wayfinder", 14),
                ("blooop/wayfinder", 7),
            ],
        );
        app.reclaimable = Some(Reclaimable::for_test(&["wf-129-closed"], 0));
        let screen = render_at(120, &app);
        let line = screen
            .lines()
            .find(|line| line.contains("stalled"))
            .unwrap_or_else(|| panic!("no stall segment: {screen}"));
        assert!(line.contains("3 stalled"), "{line}");
        assert!(
            line.contains("wf reap"),
            "the command survives beside the stalls: {line}"
        );
    }

    /// Neither segment is ever clipped to a fragment, at any width.
    ///
    /// The regression this pins was visible only on a rendered screen: with two
    /// stalls named on a 60-column terminal, the reclaim note was left three
    /// characters short of its own count and its last arm — which clips rather
    /// than vanishing — put `· 2 reclaima` on the line. Every assertion about
    /// these two segments passed, because each was measured on its own.
    ///
    /// Fragments are also how an *overrun* shows up here, and deliberately the
    /// only way it can: the frame buffer is the area's width by construction,
    /// so a line that asked for more was already truncated by the time it can
    /// be read back, and `inner.len() <= width` is a fact about `ratatui`
    /// rather than about this code. What a truncation leaves behind is a
    /// partial word at the end of the line, which is what is asserted.
    #[test]
    fn neither_count_line_segment_can_clip_the_other_into_a_fragment() {
        let mut app = fixture_app();
        app.liveness = crate::liveness::Liveness::for_test(
            &[],
            &[("blooop/wayfinder", 9), ("blooop/wayfinder", 14)],
        );
        app.reclaimable = Some(Reclaimable::for_test(
            &["devlaunch-github-blooop-wayfinder-127-ladepomi", "wf-80-x"],
            1,
        ));
        for width in 40..=130u16 {
            let screen = render_at(width, &app);
            let line = screen.lines().nth(2).expect("a count line");
            let inner: String = line.chars().skip(1).take(width as usize - 2).collect();
            // A word or nothing. Any proper prefix of "reclaimable"/"stalled"
            // left at the end of the line is the fragment this exists to catch.
            // From one character: `· 2 r` is as wrong as `· 2 reclaima`, and no
            // segment ends in a letter that could be mistaken for one — the
            // names end in digits, the asides in `)`, the pointer in `p`.
            for whole in ["reclaimable", "stalled"] {
                for cut in 1..whole.len() {
                    let fragment = &whole[..cut];
                    assert!(
                        !inner.trim_end().ends_with(fragment),
                        "{width}: {whole:?} was clipped to {fragment:?}: {:?}",
                        inner.trim_end()
                    );
                }
            }
        }
    }

    #[test]
    fn the_first_frame_says_nothing_about_reclaimable_workspaces() {
        // The reading is a `dl` subprocess and a GraphQL call behind the
        // screen. Until it lands — and forever, if it failed or found nothing —
        // the picker looks exactly as it did before #137.
        let app = fixture_app();
        assert_eq!(app.reclaimable, None, "nothing has been read yet");
        let screen = render(&app);
        assert!(
            !screen.contains("reclaimable"),
            "the picker must not mention a reading it has not got: {screen}"
        );
    }

    #[test]
    fn the_count_line_names_what_a_reap_would_claim_once_the_reading_lands() {
        // A count alone cannot be judged, so the segment names the workspaces
        // and the command that acts on them. Nothing here deletes anything —
        // the whole feature is this sentence.
        let mut app = fixture_app();
        app.reclaimable = Some(Reclaimable::for_test(&["ws-a", "ws-b"], 0));
        let screen = render(&app);
        let line = screen
            .lines()
            .find(|line| line.contains("reclaimable"))
            .unwrap_or_else(|| panic!("the count line says so: {screen}"));
        assert!(line.contains("2 reclaimable"), "{line}");
        assert!(line.contains("ws-a"), "{line}");
        assert!(line.contains("ws-b"), "{line}");
        assert!(line.contains("wf reap"), "{line}");
    }

    #[test]
    fn a_warned_workspace_is_never_drawn_as_reclaimable() {
        // #128's posture, at the last place it could be lost: the warned count
        // is an aside, and the leading number is the doomed set's alone.
        let mut app = fixture_app();
        app.reclaimable = Some(Reclaimable::for_test(&["ws-a"], 2));
        let screen = render(&app);
        let line = screen
            .lines()
            .find(|line| line.contains("reclaimable"))
            .unwrap_or_else(|| panic!("the count line says so: {screen}"));
        assert!(line.contains("1 reclaimable"), "{line}");
        assert!(line.contains("+2 to check by hand"), "{line}");
    }

    #[test]
    fn the_count_line_keeps_the_command_when_the_names_are_real_ones() {
        // `ws-a` fits anywhere and proves nothing. A `dl` workspace id is ~40
        // characters, and three of them written out unconditionally push the
        // aside and the `wf reap` pointer past the right edge of an
        // 80-column terminal — leaving a segment that names workspaces and
        // says nothing about what to do with them.
        let mut app = fixture_app();
        app.reclaimable = Some(Reclaimable::for_test(
            &[
                "devlaunch-github-com-blooop-wayfinder-129",
                "devlaunch-github-com-blooop-wayfinder-127",
                "devlaunch-github-com-blooop-wayfinder-80x",
            ],
            1,
        ));
        for width in [80, 100, 120] {
            let screen = render_at(width, &app);
            let line = screen
                .lines()
                .find(|line| line.contains("reclaimable"))
                .unwrap_or_else(|| panic!("{width}: the count line says so: {screen}"));
            assert!(line.contains("3 reclaimable"), "{width}: {line}");
            assert!(
                line.contains("(+1 to check by hand)"),
                "{width}: the aside is never what gets clipped: {line}"
            );
            assert!(
                line.contains("wf reap"),
                "{width}: nor is the command: {line}"
            );
            assert!(
                line.contains("129"),
                "{width}: and a workspace is still named: {line}"
            );
        }
    }

    /// A map the tracker could not send all of is named on the count line
    /// (#184): the body draws a perfectly normal tree either way, so this
    /// segment is the only trace that tickets — or the blocking edges their
    /// classification is drawn from — are missing from it.
    #[test]
    fn the_count_line_says_when_a_map_arrived_truncated() {
        let mut clusters = BTreeMap::new();
        let mut map = wf_map();
        map.truncated = true;
        clusters.insert(MapId::new("blooop/wayfinder", 1), map);
        let app = app_on("blooop/wayfinder", clusters);
        let screen = render(&app);
        let line = screen
            .lines()
            .find(|line| line.contains("truncated"))
            .unwrap_or_else(|| panic!("no truncation segment: {screen}"));
        assert!(
            line.contains("blooop/wayfinder#1 truncated"),
            "one truncated map is named, like one failed map is: {line}"
        );

        // A complete map says nothing — silence is the ordinary case.
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        let app = app_on("blooop/wayfinder", clusters);
        assert!(!render(&app).contains("truncated"));
    }

    #[test]
    fn several_truncated_maps_collapse_to_a_count() {
        let mut clusters = BTreeMap::new();
        let mut first = wf_map();
        first.truncated = true;
        clusters.insert(MapId::new("blooop/wayfinder", 1), first);
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: the selection view".to_string(),
                last_activity: None,
                truncated: true,
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
        let app = app_on("blooop/wayfinder", clusters);
        let screen = render(&app);
        assert!(
            screen.contains("· 2 maps truncated"),
            "several collapse to a count, like failures do: {screen}"
        );
    }

    #[test]
    fn idle_maps_drop_to_the_count_line_and_tab_brings_them_back() {
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        // A second map of the *same* project: a screen is one repo's, so an
        // idle map has to be one of this repo's to be dropped from it.
        clusters.insert(
            MapId::new("blooop/wayfinder", 4),
            Map {
                title: "Map: the archive".to_string(),
                last_activity: None,
                truncated: false,
                tickets: vec![ticket(
                    "blooop/wayfinder",
                    103,
                    "All done here",
                    false,
                    false,
                    vec![],
                )],
            },
        );
        let mut app = app_on("blooop/wayfinder", clusters);
        let screen = render(&app);
        assert!(
            !screen.contains("Map: the archive"),
            "an idle map leaves the body: {screen}"
        );
        assert!(screen.contains("· 1 idle map hidden"), "{screen}");
        // The forest is the escape hatch: everything renders there.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(
            screen.contains("▌ wayfinder · Map: the archive"),
            "{screen}"
        );
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
                truncated: false,
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
        let app = app_on("blooop/wayfinder", clusters);
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
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: the archive".to_string(),
                last_activity: Activity::parse("2026-08-06T12:00:00Z"),
                truncated: false,
                tickets: vec![ticket(
                    "blooop/wayfinder",
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
                truncated: false,
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
        let mut app = app_on("blooop/wayfinder", clusters);
        // The forest, because that is the screen a finished map appears on at
        // all — leverage drops it as idle.
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let screen = render(&app);
        let live = screen
            .find("▌ wayfinder · Map: the selection view")
            .expect("the live cluster");
        let finished = screen
            .find("▌ wayfinder · Map: the archive")
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
    fn the_chrome_has_count_prompt_and_hint_lines() {
        let screen = render(&fixture_app());
        assert!(screen.contains("5/5"));
        assert!(screen.contains("> █"));
        assert!(screen.contains("enter launch"));
        assert!(screen.contains("tab structure"));
        assert!(screen.contains("esc quit"));
        // The hint bar names only keys that do something. The refresh chord
        // used to sit between `tab structure` and `esc quit`; it is unbound
        // now, and a hint outliving its binding is a screen telling a lie.
        assert!(!screen.contains("refresh"), "{screen}");
        assert!(screen.contains("wf · blooop/wayfinder"));
    }

    #[test]
    fn the_search_prompt_leads_the_screen_with_the_count_under_it() {
        // The prompt is the first line inside the border and the count line the
        // second, so both sit against the title rather than drifting to
        // wherever the body happens to end. Only the hints stay anchored to the
        // bottom.
        let screen = render(&fixture_app());
        let lines: Vec<&str> = screen.lines().collect();
        assert!(lines[0].contains("wf · blooop/wayfinder"), "{screen}");
        assert!(lines[1].contains("> █"), "{screen}");
        assert!(lines[2].contains("5/5"), "{screen}");
        // The body follows, still opening on its own blank spacer line — which
        // now reads as the gap between the chrome and the first cluster.
        // The project row leads the body, with the spacer that separates it
        // from the first cluster — so the cluster header is two lines further
        // down than it was when the body opened straight onto it.
        assert!(lines[4].contains("▌ blooop/wayfinder"), "{screen}");
        let header = lines
            .iter()
            .position(|l| l.contains("▌ wayfinder · Map: wf"))
            .expect("cluster header on screen");
        assert_eq!(header, 6, "{screen}");
        assert!(
            lines[lines.len() - 2].contains("enter launch"),
            "hints stay on the last line: {screen}"
        );
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
                .any(|l| l.starts_with("  ▌ «wayf»inder · Map: wf")),
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
        let mut app = app_on("blooop/wayfinder", clusters);
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
            1,
            "typing snaps the cursor to the best hit, past its cluster header"
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
        let checkout = |path: &str| {
            crate::projects::Checkout::new(
                std::path::PathBuf::from(path),
                "blooop/wayfinder".to_string(),
            )
        };
        fixture_app().with_checkouts(vec![
            checkout("/data/k1/wayfinder"),
            checkout("/data/k2/wayfinder"),
        ])
    }

    #[test]
    fn the_hint_line_is_the_levels_and_advertises_no_afk() {
        let screen = render(&fixture_app());
        assert!(screen.contains("enter launch"), "{screen}");
        assert!(screen.contains("← back"), "{screen}");
        assert!(!screen.contains("ctrl-a"), "{screen}");
        assert!(!screen.contains("afk"), "{screen}");

        // On the list there is nothing to launch, nothing to descend into and
        // nowhere further back — so the line says none of those.
        let mut app = fixture_app();
        app.level = crate::app::Level::Projects;
        let screen = render(&app);
        assert!(screen.contains("enter open"), "{screen}");
        assert!(!screen.contains("← back"), "{screen}");
        assert!(!screen.contains("tab structure"), "{screen}");
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
        down(&mut app, 2); // past the project row and the cluster header, onto #6
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
    fn a_project_picker_draws_the_creation_rows_with_their_own_skills() {
        // Creation is an act on a repo, so the rows live on the one stop that
        // is a repo — and every row still names the skill it execs, including
        // the ones that launch no node.
        let mut app = fixture_app();
        assert!(
            matches!(app.cursor_stop(), Some(crate::view::Stop::Project(_))),
            "the project row is where an untouched cursor already is"
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("new task"), "{screen}");
        assert!(screen.contains("/wf-one"), "{screen}");
        assert!(screen.contains("one tracked ticket"), "{screen}");
        assert!(screen.contains("new map"), "{screen}");
        assert!(screen.contains("new map, mid"), "{screen}");
        assert!(screen.contains("new map, auto"), "{screen}");
        // The field is named for the row it fills: `task` on the first one,
        // which is the only thing telling you the text is not steering.
        assert!(screen.contains("task"), "{screen}");
        // Nothing to launch here, so no launch row names a mode.
        assert!(!screen.contains("interactive"), "{screen}");
    }

    #[test]
    fn no_mode_row_widens_the_picker_past_the_terminal_that_fits_the_rest() {
        // 83 columns is what the picker's widest row has always needed — the
        // route column plus `the agent decides alone and drives it to done`.
        // A blurb longer than that does not wrap; the popup is clamped to the
        // frame and the tail of the row is simply gone, so the row that says
        // what a mode *does* is the one that loses its ending. The `mid` row
        // arrived six columns over that budget, which clipped it on every
        // terminal between 83 and 88 columns where nothing clipped before.
        let app = {
            let mut app = fixture_app();
            down(&mut app, 2);
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            app
        };
        let screen = render_at(83, &app);
        for mode in crate::launch::Mode::all() {
            assert!(
                screen.contains(mode.blurb()),
                "{} is clipped at 83 columns:\n{screen}",
                mode.label()
            );
        }
    }

    #[test]
    fn a_ticket_picker_draws_no_creation_rows() {
        // The other half of the rule: a ticket is not a repo-level stop, so
        // its picker is the modes and nothing else.
        let app = {
            let mut app = fixture_app();
            down(&mut app, 2); // past the project row and the cluster header
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            app
        };
        let screen = render(&app);
        assert!(screen.contains("interactive"), "{screen}");
        assert!(!screen.contains("new task"), "{screen}");
        assert!(!screen.contains("new map"), "{screen}");
    }

    #[test]
    fn enter_opens_the_launch_picker_over_the_screen_with_every_mode_on_it() {
        let mut app = fixture_app();

        // Enter on #6 (a task at ready): the picker floats over the list,
        // titled with the node it launches.
        down(&mut app, 2); // past the project row and the cluster header
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let screen = render(&app);
        // The repo leads the title (#114): it is the one fact every row shares
        // once a header's rows can start something new in it.
        assert!(
            screen.contains("launch Claude · blooop/wayfinder · #6 Re-entry breadcrumbs"),
            "{screen}"
        );
        // Every mode is on screen with the skill each one would run, because
        // that difference *is* the choice being offered — and the cursor sits
        // on the interactive default.
        assert!(screen.contains("▶ interactive"), "{screen}");
        assert!(screen.contains("/wf "), "{screen}");
        assert!(screen.contains("mid"), "{screen}");
        assert!(screen.contains("/wf-mid"), "{screen}");
        assert!(screen.contains("auto"), "{screen}");
        assert!(screen.contains("/wf-auto"), "{screen}");
        // The middle row's blurb is the whole reason it is a separate mode:
        // it decides what it can and asks about what it cannot.
        assert!(screen.contains("asks what it can't"), "{screen}");
        // The skill-free mode (#112) reads as what it execs — a bare `claude`,
        // no slash command — and says what picking it costs you. The route
        // column is asserted too: it is the half of the row that says what will
        // actually run, and it is drawn from a label nothing else reads.
        assert!(screen.contains("plain"), "{screen}");
        assert!(screen.contains("claude"), "{screen}");
        assert!(screen.contains("no skill"), "{screen}");
        assert!(screen.contains("steer"), "{screen}");
        assert!(screen.contains("←/→ agent"), "{screen}");
        assert!(screen.contains("esc cancel"), "{screen}");

        // Horizontal movement changes the agent named in the title and the
        // invocation syntax together; the vertically selected mode stays put.
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(
            screen.contains("launch Codex · blooop/wayfinder · #6 Re-entry breadcrumbs"),
            "{screen}"
        );
        assert!(screen.contains("$wf "), "{screen}");
        assert!(screen.contains("$wf-mid"), "{screen}");
        assert!(screen.contains("$wf-auto"), "{screen}");

        // Down moves the pick; the marker moves with it and nothing about the
        // route line is stale, since each row draws its own.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▶ mid"), "{screen}");
        assert!(!screen.contains("▶ interactive"), "{screen}");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▶ auto"), "{screen}");
        assert!(!screen.contains("▶ mid"), "{screen}");
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(screen.contains("▶ plain"), "{screen}");
        assert!(!screen.contains("▶ auto"), "{screen}");
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        // Typing steers rather than picking: the text lands in the field, and
        // the mode stays where the cursor put it.
        type_str(&mut app, "merge when green");
        let screen = render(&app);
        assert!(screen.contains("merge when green"), "{screen}");
        assert!(screen.contains("▶ auto"), "{screen}");

        // The count line was never taken away — the picker covers the rows, not
        // the chrome that says how many there are.
        assert!(screen.contains("5/5"), "{screen}");

        // Esc closes it and gives the whole screen back.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let screen = render(&app);
        assert!(!screen.contains("launch Claude"), "{screen}");
        assert!(!screen.contains("launch Codex"), "{screen}");
        assert!(screen.contains("5/5"), "{screen}");
    }

    #[test]
    fn the_first_frame_says_it_is_loading_rather_than_that_there_is_nothing() {
        // The whole point of #27: this screen is drawn before any network call,
        // so its empty list must not read as "no tickets" or "no projects".
        let mut app = App::empty();
        app.enter("blooop/wayfinder");
        let screen = render(&app);
        assert!(screen.contains("searching for maps…"), "{screen}");
        // The screen is this project's from the first frame — the level came
        // from a local `git` call, not from the fetch — and its row says the
        // maps are still coming rather than that there are none.
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
        assert!(screen.contains("▌ blooop/wayfinder · loading…"), "{screen}");
        assert!(!screen.contains("no map"), "{screen}");
    }

    #[test]
    fn an_empty_list_after_a_failed_fetch_does_not_claim_there_are_no_projects() {
        // Three registered projects and GitHub unreachable draws the same empty
        // list as no projects at all. Saying "no projects — run wf inside a
        // checkout" there sends the user to fix the one thing that is not
        // broken, so the failure has to win the heading — and it names the
        // *map*, because with several on one repo the repo alone is ambiguous.
        let mut app = App::empty().with_checkouts(vec![
            crate::projects::Checkout::new(
                std::path::PathBuf::from("/data/proj/wayfinder"),
                "blooop/wayfinder".to_string(),
            ),
            crate::projects::Checkout::new(
                std::path::PathBuf::from("/data/proj/dotfiles"),
                "blooop/dotfiles".to_string(),
            ),
        ]);
        app.startup = Startup::loaded();
        app.failed.insert(MapId::new("blooop/wayfinder", 35));
        let screen = render(&app);
        assert!(!screen.contains("no projects"), "{screen}");
        assert!(
            screen.contains("blooop/wayfinder#35 — fetch failed, run wf again"),
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
                truncated: false,
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
                truncated: false,
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
        let mut app = app_on("blooop/wayfinder", clusters);
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.cursor_ticket().unwrap().number, 50);
        let screen = render(&app);
        assert!(screen.contains("▶ ○ #50 Build: clusters"), "{screen}");
        // And with the cursor back at the top — the project's own row, which
        // is the first stop of its screen — the top is what shows.
        for _ in 0..40 {
            app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        let screen = render(&app);
        assert!(screen.contains("▶ ▌ blooop/wayfinder"), "{screen}");
        assert!(screen.contains("○ #1 Open 1"), "{screen}");
    }

    #[test]
    fn the_project_list_draws_a_row_per_project_with_what_is_inside_it() {
        // The top-level screen: full slugs (two forks of one repo have the same
        // short name and sit next to each other here), most recently used
        // first, each saying how much is inside it in the same glyph vocabulary
        // the rows below use.
        let mut clusters = BTreeMap::new();
        clusters.insert(MapId::new("blooop/wayfinder", 1), wf_map());
        let stamped = |path: &str, repo: &str, at: u64| crate::projects::Checkout {
            path: std::path::PathBuf::from(path),
            repo: repo.to_string(),
            used: Some(at),
        };
        let mut app = App::new(clusters).with_checkouts(vec![
            stamped("/data/proj/wayfinder", "blooop/wayfinder", 200),
            stamped("/data/proj/newthing", "blooop/newthing", 100),
        ]);
        app.startup = Startup::loaded();
        let screen = render(&app);
        assert!(screen.contains("wf · 2 projects"), "{screen}");
        assert!(
            !screen.contains("← all projects"),
            "nowhere further out to go"
        );

        let lines: Vec<&str> = screen.lines().collect();
        let wayfinder = lines
            .iter()
            .position(|l| l.contains("▌ blooop/wayfinder · 1 map"))
            .expect("the mapped project");
        let newthing = lines
            .iter()
            .position(|l| l.contains("▌ blooop/newthing · no map — enter to start one"))
            .expect("the map-less project — a row here like any other");
        assert!(wayfinder < newthing, "most recently used first: {screen}");
        // The counts are the map's, in the glyphs the rows use.
        assert!(lines[wayfinder].contains("○1"), "{screen}");
        // And no ticket got onto this screen.
        assert!(!screen.contains("#6 Re-entry breadcrumbs"), "{screen}");
    }

    #[test]
    fn a_project_screen_names_its_project_and_the_way_back() {
        let screen = render(&fixture_app());
        assert!(screen.contains("wf · blooop/wayfinder"), "{screen}");
        // The way back is named on the screen it applies to, and it is the key
        // that does it — the chords that used to focus and widen are gone.
        assert!(screen.contains("← all projects"), "{screen}");
        assert!(!screen.contains("ctrl-f"), "{screen}");
        assert!(!screen.contains("ctrl-g"), "{screen}");
    }
}
