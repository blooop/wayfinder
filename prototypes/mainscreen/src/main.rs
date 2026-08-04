//! Throwaway prototype for wayfinder issue #9: "Main screen design".
//!
//! Renders static screens of the fuzzy picker over the grouped multi-project
//! list (form decided in #8: frontier / claimed / blocked / done, repo on
//! every row). Fake but realistic data: 3 repos (wayfinder, dotfiles, kinisi
//! with two checkouts k1/k2), 16 tickets in mixed states.
//!
//! Screens (all ~100x35, dumped through ratatui's TestBackend, no terminal
//! needed — `cargo run` prints everything):
//!   1. default global view (grouped, fuzzy input at bottom, AFK slot reserved)
//!   2a. mid-query "bread" — groups preserved, non-matches dropped
//!   2b. mid-query "bread" — flattened into one ranked list (real nucleo scores)
//!   3. cwd/focus mode — scoped to one project
//!   4. launch prompt for a ticket whose repo has two checkouts (k1/k2)
//!   5. row anatomy with all optional fields ON (blocks badge, age, assignee)

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

const W: usize = 100;
const H: usize = 35;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Frontier,
    Claimed,
    Blocked,
    Done,
}

struct Ticket {
    repo: &'static str,
    num: u32,
    title: &'static str,
    state: State,
    needs: &'static [u32],     // unresolved blockers (same repo)
    blocks: u32,               // open tickets blocked on this one (hub badge)
    age: &'static str,         // time since last activity
    assignee: &'static str,    // "" = unassigned
}

fn t(
    repo: &'static str,
    num: u32,
    title: &'static str,
    state: State,
    needs: &'static [u32],
    blocks: u32,
    age: &'static str,
    assignee: &'static str,
) -> Ticket {
    Ticket { repo, num, title, state, needs, blocks, age, assignee }
}

/// 3 projects, 2 with one checkout + 1 with two checkouts; 16 tickets.
fn tickets() -> Vec<Ticket> {
    use State::*;
    vec![
        // wayfinder — checkout ~/ws/wayfinder
        t("wayfinder", 4, "How does wf discover projects and maps?", Done, &[], 0, "1d", "blooop"),
        t("wayfinder", 6, "Re-entry breadcrumbs", Frontier, &[], 3, "2d", ""),
        t("wayfinder", 7, "Supervising detached AFK agents", Blocked, &[6], 0, "21d", ""),
        t("wayfinder", 9, "Main screen design: fuzzy picker over the grouped list", Claimed, &[], 0, "3h", "blooop"),
        t("wayfinder", 10, "Ticket peek: preview pane", Frontier, &[], 0, "6d", ""),
        t("wayfinder", 14, "Breadcrumb protocol: structured markers", Blocked, &[6], 1, "5d", ""),
        t("wayfinder", 16, "Packaging: rattler-build recipe on prefix.dev", Frontier, &[], 0, "1d", ""),
        // dotfiles — checkout ~/dotfiles
        t("dotfiles", 97, "fzf keybindings audit", Done, &[], 0, "12d", "blooop"),
        t("dotfiles", 103, "Prune legacy bash aliases", Frontier, &[], 0, "30d", ""),
        t("dotfiles", 112, "Migrate kitty config to chezmoi template", Frontier, &[], 0, "8d", ""),
        t("dotfiles", 118, "zellij: session-per-project layout preset", Claimed, &[], 0, "1d", "blooop"),
        // kinisi — checkouts ~/kinisi/k1 and ~/kinisi/k2
        t("kinisi", 79, "Sensor fusion spike", Done, &[], 0, "20d", "blooop"),
        t("kinisi", 84, "Breaker panel firmware update", Claimed, &[], 0, "6h", "blooop"),
        t("kinisi", 88, "Board bring-up readiness checklist", Frontier, &[], 1, "4d", ""),
        t("kinisi", 91, "Motor calibration harness", Blocked, &[88], 0, "14d", ""),
        t("kinisi", 95, "CAN bus logger", Frontier, &[], 0, "9d", ""),
    ]
}

fn glyph(s: State) -> char {
    match s {
        State::Frontier => '○',
        State::Claimed => '◐',
        State::Blocked => '⊘',
        State::Done => '●',
    }
}

const GROUPS: [(State, &str); 4] = [
    (State::Frontier, "FRONTIER — ready to claim"),
    (State::Claimed, "CLAIMED"),
    (State::Blocked, "BLOCKED"),
    (State::Done, "DONE"),
];

// ---------------------------------------------------------------------------
// text plumbing: char-aware pad, frame, overlay, TestBackend dump
// ---------------------------------------------------------------------------

fn pad(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    out.push_str(&" ".repeat(w - n));
    out
}

/// Wrap content lines in a box border of exactly `w` x `h` cells.
fn frame(title_l: &str, title_r: &str, content: &[String], w: usize, h: usize) -> Vec<String> {
    let inner = w - 2;
    let tl = format!(" {title_l} ");
    let tr = if title_r.is_empty() { String::new() } else { format!(" {title_r} ") };
    let dashes = inner
        .saturating_sub(1 + tl.chars().count() + tr.chars().count() + 1);
    let mut out = Vec::with_capacity(h);
    out.push(format!("┌─{tl}{}{tr}─┐", "─".repeat(dashes)));
    for i in 0..h - 2 {
        let line = content.get(i).map(String::as_str).unwrap_or("");
        out.push(format!("│{}│", pad(line, inner)));
    }
    out.push(format!("└{}┘", "─".repeat(inner)));
    out
}

/// Splice `boxed` lines into `base` at (x, y) — used for the launch modal.
fn overlay(base: &mut [String], boxed: &[String], x: usize, y: usize) {
    for (i, bl) in boxed.iter().enumerate() {
        let row: Vec<char> = base[y + i].chars().collect();
        let bw = bl.chars().count();
        let mut new: String = row[..x].iter().collect();
        new.push_str(bl);
        new.extend(row[(x + bw).min(row.len())..].iter());
        base[y + i] = new;
    }
}

fn dump_via_ratatui(lines: &[String]) -> String {
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as u16;
    let h = lines.len() as u16;
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let text: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
    terminal
        .draw(|f| f.render_widget(Paragraph::new(text), f.area()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// row + screen builders
// ---------------------------------------------------------------------------

/// One list row. `cursor` renders the fzf-style pointer; `show_repo` is off in
/// cwd-focus mode (the scope line already names the project).
fn row(tk: &Ticket, cursor: bool, show_repo: bool) -> String {
    let cur = if cursor { '▶' } else { ' ' };
    let needs = if tk.needs.is_empty() {
        String::new()
    } else {
        let n: Vec<String> = tk.needs.iter().map(|b| format!("#{b}")).collect();
        format!("  — needs {}", n.join(", "))
    };
    if show_repo {
        format!("  {cur} {} {} #{:<4} {}{needs}", glyph(tk.state), pad(tk.repo, 10), tk.num, tk.title)
    } else {
        format!("  {cur} {} #{:<4} {}{needs}", glyph(tk.state), tk.num, tk.title)
    }
}

/// Grouped list body. `keep` filters rows; group headers show n or n/total.
fn grouped_body(
    all: &[Ticket],
    keep: impl Fn(&Ticket) -> bool,
    cursor_on: Option<(&str, u32)>,
    show_repo: bool,
    filtered: bool,
) -> Vec<String> {
    let mut lines = vec![String::new()];
    for (state, label) in GROUPS {
        let total = all.iter().filter(|t| t.state == state).count();
        let hits: Vec<&Ticket> =
            all.iter().filter(|t| t.state == state && keep(t)).collect();
        if filtered {
            lines.push(format!("  {label} — {}/{}", hits.len(), total));
        } else {
            lines.push(format!("  {label} — {total}"));
        }
        for tk in &hits {
            let cur = cursor_on == Some((tk.repo, tk.num));
            lines.push(row(tk, cur, show_repo));
        }
        lines.push(String::new());
    }
    lines.pop();
    lines
}

/// Full 100x35 screen: header in the border, list body, then anchored bottom
/// chrome — reserved AFK slot, match count, fzf-style prompt, key hints.
fn list_screen(
    title_l: &str,
    title_r: &str,
    body: Vec<String>,
    count: &str,
    query: &str,
) -> Vec<String> {
    let inner_h = H - 2;
    let bottom = vec![
        format!("  ┄┄ AFK agents ┄ (slot reserved — supervision not designed yet, see #7) {}", "┄".repeat(22)),
        format!("  {count}"),
        format!("  > {query}█"),
        "  enter launch · tab peek · ctrl-f focus row's project · ctrl-g all · ctrl-r refresh · esc quit"
            .to_string(),
    ];
    let mut content = body;
    while content.len() < inner_h - bottom.len() {
        content.push(String::new());
    }
    content.truncate(inner_h - bottom.len());
    content.extend(bottom);
    frame(title_l, title_r, &content, W, H)
}

// ---------------------------------------------------------------------------
// nucleo scoring
// ---------------------------------------------------------------------------

/// Real nucleo score of `query` against "repo #num title" (so typing a repo
/// name also narrows — one possible answer to the `repo:` scoping question).
fn scores(all: &[Ticket], query: &str) -> Vec<(usize, u32)> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pat = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut hits: Vec<(usize, u32)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, tk)| {
            let hay = format!("{} #{} {}", tk.repo, tk.num, tk.title);
            pat.score(Utf32Str::new(&hay, &mut buf), &mut matcher)
                .map(|s| (i, s))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));
    hits
}

// ---------------------------------------------------------------------------
// screens
// ---------------------------------------------------------------------------

fn screen1_default(all: &[Ticket]) -> Vec<String> {
    let body = grouped_body(all, |_| true, Some(("wayfinder", 6)), true, false);
    list_screen(
        "wf · all projects (3)",
        "synced 3s ago",
        body,
        &format!("{}/{}", all.len(), all.len()),
        "",
    )
}

fn screen2a_query_grouped(all: &[Ticket], query: &str) -> Vec<String> {
    let hits = scores(all, query);
    let keep_idx: Vec<usize> = hits.iter().map(|(i, _)| *i).collect();
    let first = hits
        .iter()
        .map(|(i, _)| &all[*i])
        .min_by_key(|tk| GROUPS.iter().position(|(s, _)| *s == tk.state))
        .map(|tk| (tk.repo, tk.num));
    let body = grouped_body(
        all,
        |tk| keep_idx.iter().any(|&i| std::ptr::eq(&all[i], tk)),
        first,
        true,
        true,
    );
    list_screen(
        "wf · all projects (3)",
        "synced 3s ago",
        body,
        &format!("{}/{}", hits.len(), all.len()),
        query,
    )
}

fn screen2b_query_flat(all: &[Ticket], query: &str) -> (Vec<String>, Vec<String>) {
    let hits = scores(all, query);
    let mut body = vec![String::new(), "  MATCHES — ranked by nucleo score".to_string()];
    let mut score_notes = Vec::new();
    for (rank, (i, s)) in hits.iter().enumerate() {
        let tk = &all[*i];
        let mut r = row(tk, rank == 0, true);
        // right-aligned group tag so state grouping survives as a column
        let tag = match tk.state {
            State::Frontier => "frontier",
            State::Claimed => "claimed",
            State::Blocked => "blocked",
            State::Done => "done",
        };
        let target = W - 2 - 12;
        let rl = r.chars().count();
        if rl < target {
            r.push_str(&" ".repeat(target - rl));
        }
        r.push_str(tag);
        body.push(r);
        score_notes.push(format!("{} #{} — nucleo score {}", tk.repo, tk.num, s));
    }
    let screen = list_screen(
        "wf · all projects (3)",
        "synced 3s ago",
        body,
        &format!("{}/{}", hits.len(), all.len()),
        query,
    );
    (screen, score_notes)
}

fn screen3_focus(all: &[Ticket]) -> Vec<String> {
    let scoped: Vec<Ticket> = tickets()
        .into_iter()
        .filter(|tk| tk.repo == "kinisi")
        .collect();
    let _ = all;
    let body = grouped_body(&scoped, |_| true, Some(("kinisi", 88)), false, false);
    list_screen(
        "wf · kinisi — cwd scope (~/kinisi/k1)",
        "ctrl-g all projects",
        body,
        &format!("{}/{}", scoped.len(), scoped.len()),
        "",
    )
}

fn screen4_launch_prompt(all: &[Ticket]) -> Vec<String> {
    // base: default view with the cursor on kinisi #88, mid-launch
    let body = grouped_body(all, |_| true, Some(("kinisi", 88)), true, false);
    let mut base = list_screen(
        "wf · all projects (3)",
        "synced 3s ago",
        body,
        &format!("{}/{}", all.len(), all.len()),
        "",
    );
    let modal_content = vec![
        String::new(),
        "  kinisi has 2 checkouts — where does this session live?".to_string(),
        String::new(),
        "  ▶ k1   ~/kinisi/k1   zellij kinisi-k1 · running · tab wf#88 exists  → re-enter".to_string(),
        "    k2   ~/kinisi/k2   zellij kinisi-k2 · not running                 → create + tab".to_string(),
        String::new(),
        "  enter attach · esc cancel".to_string(),
    ];
    let modal = frame(
        "launch kinisi #88 — Board bring-up readiness checklist",
        "",
        &modal_content,
        88,
        9,
    );
    overlay(&mut base, &modal, 6, 12);
    base
}

fn screen5_row_anatomy(all: &[Ticket]) -> Vec<String> {
    let pick = |repo: &str, num: u32| all.iter().find(|t| t.repo == repo && t.num == num).unwrap();
    let examples = [
        pick("wayfinder", 6),
        pick("dotfiles", 103),
        pick("wayfinder", 9),
        pick("kinisi", 91),
        pick("dotfiles", 97),
    ];
    let mut content = vec![
        String::new(),
        format!("{}{}  {}  {}", " ".repeat(23), pad("", 42), pad("blocks", 6), "age   assignee"),
    ];
    for (i, tk) in examples.iter().enumerate() {
        let cur = if i == 0 { '▶' } else { ' ' };
        let mut title = tk.title.to_string();
        if !tk.needs.is_empty() {
            let n: Vec<String> = tk.needs.iter().map(|b| format!("#{b}")).collect();
            title.push_str(&format!("  — needs {}", n.join(", ")));
        }
        if title.chars().count() > 42 {
            title = format!("{}…", title.chars().take(41).collect::<String>());
        }
        let badge = if tk.blocks > 0 { format!("⤷{}", tk.blocks) } else { "·".to_string() };
        let assignee = if tk.assignee.is_empty() { "·".to_string() } else { format!("@{}", tk.assignee) };
        content.push(format!(
            "  {cur} {} {} #{:<4} {}  {}  {}  {}",
            glyph(tk.state),
            pad(tk.repo, 10),
            tk.num,
            pad(&title, 42),
            pad(&badge, 6),
            pad(tk.age, 4),
            assignee
        ));
    }
    content.extend([
        String::new(),
        "  ⤷N       hub badge: N open tickets are blocked on this one (hidden when 0)".to_string(),
        "  age      time since last activity on the ticket".to_string(),
        "  assignee claim owner — today every claim is @blooop; earns its column only when that changes?"
            .to_string(),
    ]);
    frame("row anatomy — every optional field ON", "", &content, W, 14)
}

// ---------------------------------------------------------------------------

fn main() {
    let all = tickets();
    let sep = "=".repeat(W);

    println!("{sep}");
    println!("SCREEN 1 — default global view");
    println!("grouped list (#8 form), repo on every row, fzf-style input line at the bottom,");
    println!("cursor on a frontier row, one-line AFK slot reserved above the prompt.");
    println!("legend: ○ frontier   ◐ claimed   ⊘ blocked   ● done");
    println!("{sep}");
    print!("{}", dump_via_ratatui(&screen1_default(&all)));

    let query = "bread";
    println!("\n{sep}");
    println!("SCREEN 2a — typing \"{query}\": GROUPS PRESERVED");
    println!("non-matching rows drop out, group headers show matched/total, empty groups stay as counts.");
    println!("{sep}");
    print!("{}", dump_via_ratatui(&screen2a_query_grouped(&all, query)));

    let (flat, notes) = screen2b_query_flat(&all, query);
    println!("\n{sep}");
    println!("SCREEN 2b — typing \"{query}\": FLATTENED to one ranked list");
    println!("order is the real nucleo score over \"repo #num title\"; state survives as glyph + right tag.");
    println!("scores: {}", notes.join(" · "));
    println!("{sep}");
    print!("{}", dump_via_ratatui(&flat));

    println!("\n{sep}");
    println!("SCREEN 3 — cwd/focus mode (wf opened inside ~/kinisi/k1)");
    println!("same screen scoped to one project; repo column dropped since the scope names it.");
    println!("{sep}");
    print!("{}", dump_via_ratatui(&screen3_focus(&all)));

    println!("\n{sep}");
    println!("SCREEN 4 — launch prompt when the repo has two checkouts");
    println!("enter on kinisi #88: pick which checkout's zellij session hosts tab wf#88.");
    println!("{sep}");
    print!("{}", dump_via_ratatui(&screen4_launch_prompt(&all)));

    println!("\n{sep}");
    println!("SCREEN 5 — row anatomy variant: every optional field ON");
    println!("blocks-count badge on the hub ticket, age, assignee — react to what stays.");
    println!("{sep}");
    print!("{}", dump_via_ratatui(&screen5_row_anatomy(&all)));
}
