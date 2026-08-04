//! Throwaway prototype for wayfinder issue #8: "Starmap legibility".
//!
//! Renders two maps (the real wayfinder map, fetched 2026-08-04 and hardcoded
//! below, plus a synthetic 25-ticket map with 16 blocking edges) in three forms:
//!   1. indented tree with edge annotations
//!   2. layered left-to-right DAG in box-drawing characters
//!   3. grouped flat list (frontier / claimed / blocked / done)
//!
//! Everything is rendered through ratatui's TestBackend (in-memory buffer) and
//! dumped as text, so `cargo run` prints all six renders with no terminal UI.

use ratatui::backend::TestBackend;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Done,
    Claimed,
    Open,
}

struct Ticket {
    id: u32,
    title: &'static str,
    state: State,
    blocked_by: Vec<u32>,
}

fn t(id: u32, title: &'static str, state: State, blocked_by: &[u32]) -> Ticket {
    Ticket { id, title, state, blocked_by: blocked_by.to_vec() }
}

// ---------------------------------------------------------------------------
// Map data
// ---------------------------------------------------------------------------

/// The real wayfinder map (issue #1's sub-issues), states as of 2026-08-04.
fn real_map() -> Vec<Ticket> {
    use State::*;
    vec![
        t(2, "Choose the implementation stack", Done, &[]),
        t(3, "GitHub Issues as the live data plane", Done, &[]),
        t(4, "How does wf discover projects and maps?", Open, &[]),
        t(5, "Prove the zellij launch seam", Claimed, &[]),
        t(6, "Re-entry breadcrumbs", Open, &[]),
        t(7, "Supervising detached AFK agents", Open, &[3]),
        t(8, "Starmap legibility", Claimed, &[2]),
    ]
}

/// Synthetic 25-ticket map, 16 blocking edges, mixed states.
/// Shaped like a plausible mid-size project: a wide frontier of roots,
/// a few chains, fan-out from a hub (tui-shell), fan-in to a release ticket.
fn synthetic_map() -> Vec<Ticket> {
    use State::*;
    vec![
        t(101, "schema", Done, &[]),
        t(102, "auth", Done, &[]),
        t(103, "parser", Claimed, &[101]),
        t(104, "cli-args", Done, &[]),
        t(105, "config", Done, &[]),
        t(106, "logging", Open, &[]),
        t(107, "api-client", Done, &[102]),
        t(108, "cache", Claimed, &[107]),
        t(109, "retry", Open, &[107]),
        t(110, "tui-shell", Claimed, &[105]),
        t(111, "tree-view", Open, &[110]),
        t(112, "dag-view", Open, &[110, 103]),
        t(113, "list-view", Open, &[110]),
        t(114, "keymap", Open, &[]),
        t(115, "launch", Open, &[105]),
        t(116, "attach", Open, &[115]),
        t(117, "breadcrumbs", Open, &[]),
        t(118, "supervise", Open, &[116, 109]),
        t(119, "poll-loop", Open, &[108]),
        t(120, "rate-limit", Open, &[]),
        t(121, "picker", Open, &[113]),
        t(122, "session", Open, &[]),
        t(123, "docs", Open, &[]),
        t(124, "packaging", Open, &[]),
        t(125, "release", Open, &[118]),
    ]
}

// ---------------------------------------------------------------------------
// Derived state + glyphs
// ---------------------------------------------------------------------------

fn by_id(tickets: &[Ticket]) -> HashMap<u32, usize> {
    tickets.iter().enumerate().map(|(i, t)| (t.id, i)).collect()
}

/// A ticket is blocked if it is not done/claimed-done and any blocker is not Done.
fn is_blocked(t: &Ticket, tickets: &[Ticket], idx: &HashMap<u32, usize>) -> bool {
    t.state != State::Done
        && t.blocked_by
            .iter()
            .any(|b| tickets[idx[b]].state != State::Done)
}

fn unresolved_blockers(t: &Ticket, tickets: &[Ticket], idx: &HashMap<u32, usize>) -> Vec<u32> {
    t.blocked_by
        .iter()
        .copied()
        .filter(|b| tickets[idx[b]].state != State::Done)
        .collect()
}

fn glyph(t: &Ticket, tickets: &[Ticket], idx: &HashMap<u32, usize>) -> char {
    if is_blocked(t, tickets, idx) {
        '⊘'
    } else {
        match t.state {
            State::Done => '●',
            State::Claimed => '◐',
            State::Open => '○',
        }
    }
}

const LEGEND: &str = "legend: ● done   ◐ claimed   ○ open (frontier)   ⊘ blocked";

// ---------------------------------------------------------------------------
// Form 1: indented tree with edge annotations
// ---------------------------------------------------------------------------
// Tree edge = first blocker (a spanning tree of the DAG). Extra blockers are
// annotated inline, since a tree cannot show fan-in natively.

fn render_tree(tickets: &[Ticket]) -> Vec<String> {
    let idx = by_id(tickets);
    // children[parent_id] = tickets whose FIRST blocker is parent_id
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();
    for t in tickets {
        match t.blocked_by.first() {
            Some(&p) => children.entry(p).or_default().push(t.id),
            None => roots.push(t.id),
        }
    }
    roots.sort();
    for v in children.values_mut() {
        v.sort();
    }

    let mut lines = Vec::new();
    fn emit(
        id: u32,
        prefix: &str,
        branch: &str,
        child_prefix: &str,
        tickets: &[Ticket],
        idx: &HashMap<u32, usize>,
        children: &HashMap<u32, Vec<u32>>,
        lines: &mut Vec<String>,
    ) {
        let t = &tickets[idx[&id]];
        let mut annot = String::new();
        if t.blocked_by.len() > 1 {
            let extra: Vec<String> =
                t.blocked_by[1..].iter().map(|b| format!("#{b}")).collect();
            annot = format!("  (also needs {})", extra.join(", "));
        }
        lines.push(format!(
            "{prefix}{branch}{} #{} {}{annot}",
            glyph(t, tickets, idx),
            t.id,
            t.title
        ));
        if let Some(kids) = children.get(&id) {
            for (i, &kid) in kids.iter().enumerate() {
                let last = i == kids.len() - 1;
                let (b, cp) = if last { ("└─ ", "   ") } else { ("├─ ", "│  ") };
                let np = format!("{prefix}{child_prefix}");
                emit(kid, &np, b, cp, tickets, idx, children, lines);
            }
        }
    }
    for &r in &roots {
        emit(r, "", "", "", tickets, &idx, &children, &mut lines);
    }
    lines
}

// ---------------------------------------------------------------------------
// Form 2: layered left-to-right DAG in box-drawing characters
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
enum Item {
    Node(usize),      // index into tickets
    Pass(usize),      // pass-through for edge index (edges spanning >1 layer)
}

fn render_dag(tickets: &[Ticket]) -> Vec<String> {
    let idx = by_id(tickets);
    let n = tickets.len();

    // edges: (blocker_index, blocked_index)
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (vi, t) in tickets.iter().enumerate() {
        for b in &t.blocked_by {
            edges.push((idx[b], vi));
        }
    }

    // layer = longest path from a root
    let mut layer = vec![0usize; n];
    let mut changed = true;
    while changed {
        changed = false;
        for &(u, v) in &edges {
            if layer[v] < layer[u] + 1 {
                layer[v] = layer[u] + 1;
                changed = true;
            }
        }
    }
    let nlayers = layer.iter().max().copied().unwrap_or(0) + 1;

    // items per layer: real nodes plus pass-throughs for long edges
    let mut layers: Vec<Vec<Item>> = vec![Vec::new(); nlayers];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| tickets[i].id);
    for &i in &order {
        layers[layer[i]].push(Item::Node(i));
    }
    for (ei, &(u, v)) in edges.iter().enumerate() {
        for l in (layer[u] + 1)..layer[v] {
            layers[l].push(Item::Pass(ei));
        }
    }

    // segments per gutter g (between layer g and g+1): (src_item, dst_item, edge)
    let seg = |layers: &Vec<Vec<Item>>| -> Vec<Vec<(usize, usize)>> {
        let mut gutters: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nlayers.saturating_sub(1)];
        for (ei, &(u, v)) in edges.iter().enumerate() {
            let (lu, lv) = (layer[u], layer[v]);
            for g in lu..lv {
                let src = if g == lu { Item::Node(u) } else { Item::Pass(ei) };
                let dst = if g + 1 == lv { Item::Node(v) } else { Item::Pass(ei) };
                let si = layers[g].iter().position(|it| *it == src).unwrap();
                let di = layers[g + 1].iter().position(|it| *it == dst).unwrap();
                gutters[g].push((si, di));
            }
        }
        gutters
    };

    // barycenter ordering: two forward sweeps, one backward
    for pass in 0..3 {
        let gutters = seg(&layers);
        if pass % 2 == 0 {
            for l in 1..nlayers {
                let mut keyed: Vec<(f64, Item)> = layers[l]
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let preds: Vec<usize> = gutters[l - 1]
                            .iter()
                            .filter(|(_, d)| *d == i)
                            .map(|(s, _)| *s)
                            .collect();
                        let key = if preds.is_empty() {
                            i as f64
                        } else {
                            preds.iter().sum::<usize>() as f64 / preds.len() as f64
                        };
                        (key, it.clone())
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                layers[l] = keyed.into_iter().map(|(_, it)| it).collect();
            }
        } else {
            for l in (0..nlayers - 1).rev() {
                let mut keyed: Vec<(f64, Item)> = layers[l]
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let succs: Vec<usize> = gutters[l]
                            .iter()
                            .filter(|(s, _)| *s == i)
                            .map(|(_, d)| *d)
                            .collect();
                        let key = if succs.is_empty() {
                            i as f64
                        } else {
                            succs.iter().sum::<usize>() as f64 / succs.len() as f64
                        };
                        (key, it.clone())
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                layers[l] = keyed.into_iter().map(|(_, it)| it).collect();
            }
        }
    }
    let gutters = seg(&layers);

    // geometry
    let label = |i: usize| -> String {
        let t = &tickets[i];
        let mut short: String = t.title.chars().take(15).collect();
        if t.title.chars().count() > 15 {
            short.push('…');
        }
        format!("{} #{} {}", glyph(t, tickets, &idx), t.id, short)
    };
    let width: Vec<usize> = layers
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|it| match it {
                    Item::Node(i) => label(*i).chars().count(),
                    Item::Pass(_) => 1,
                })
                .max()
                .unwrap_or(1)
        })
        .collect();
    let mut x = vec![0usize; nlayers];
    for l in 1..nlayers {
        let gw = gutters[l - 1].len() + 2; // 1 pad + channels + 1 pad(arrow)
        x[l] = x[l - 1] + width[l - 1] + gw;
    }
    let grid_h = layers.iter().map(|it| it.len() * 2).max().unwrap_or(1);
    let grid_w = x[nlayers - 1] + width[nlayers - 1];
    let mut grid = vec![vec![' '; grid_w]; grid_h];

    let put = |grid: &mut Vec<Vec<char>>, gx: usize, gy: usize, c: char| {
        let old = grid[gy][gx];
        grid[gy][gx] = match (old, c) {
            (' ', _) => c,
            (o, n) if o == n => o,
            ('─', '│') | ('│', '─') => '┼',
            (o, n) if "─│╮╯╭╰┼".contains(o) && "─│╮╯╭╰┼".contains(n) => '┼',
            (_, n) => n,
        };
    };

    // draw edges (per gutter), channels assigned by source row
    for g in 0..gutters.len() {
        let mut segs: Vec<(usize, (usize, usize))> =
            gutters[g].iter().copied().enumerate().collect();
        segs.sort_by_key(|&(_, (s, d))| (s, d));
        let gx0 = x[g] + width[g]; // gutter start
        for (k, (_, (si, di))) in segs.iter().enumerate() {
            let ys = si * 2;
            let yd = di * 2;
            let xc = gx0 + 1 + k;
            let exit_x = match &layers[g][*si] {
                Item::Node(i) => x[g] + label(*i).chars().count(),
                Item::Pass(_) => x[g] + width[g],
            };
            let entry_arrow = matches!(&layers[g + 1][*di], Item::Node(_));
            let x_next = x[g + 1];
            if ys == yd {
                for gx in exit_x..x_next - 1 {
                    put(&mut grid, gx, ys, '─');
                }
            } else {
                for gx in exit_x..xc {
                    put(&mut grid, gx, ys, '─');
                }
                put(&mut grid, xc, ys, if yd > ys { '╮' } else { '╯' });
                let (a, b) = if yd > ys { (ys + 1, yd) } else { (yd + 1, ys) };
                for gy in a..b {
                    put(&mut grid, xc, gy, '│');
                }
                put(&mut grid, xc, yd, if yd > ys { '╰' } else { '╭' });
                for gx in (xc + 1)..x_next - 1 {
                    put(&mut grid, gx, yd, '─');
                }
            }
            if entry_arrow {
                grid[yd][x_next - 1] = '▶';
            } else {
                put(&mut grid, x_next - 1, yd, '─');
            }
        }
    }

    // draw pass-throughs (a horizontal line across the whole column)
    for l in 0..nlayers {
        for (ri, it) in layers[l].iter().enumerate() {
            if matches!(it, Item::Pass(_)) {
                for gx in x[l]..x[l] + width[l] {
                    put(&mut grid, gx, ri * 2, '─');
                }
            }
        }
    }

    // draw node labels last so they stay clean
    for l in 0..nlayers {
        for (ri, it) in layers[l].iter().enumerate() {
            if let Item::Node(i) = it {
                for (ci, c) in label(*i).chars().enumerate() {
                    grid[ri * 2][x[l] + ci] = c;
                }
            }
        }
    }

    grid.into_iter()
        .map(|row| row.into_iter().collect::<String>().trim_end().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Form 3: grouped flat list — no graph at all
// ---------------------------------------------------------------------------

fn render_grouped(tickets: &[Ticket]) -> Vec<String> {
    let idx = by_id(tickets);
    let mut frontier = Vec::new();
    let mut claimed = Vec::new();
    let mut blocked = Vec::new();
    let mut done = Vec::new();
    let mut order: Vec<&Ticket> = tickets.iter().collect();
    order.sort_by_key(|t| t.id);
    for t in order {
        let g = glyph(t, tickets, &idx);
        match (t.state, is_blocked(t, tickets, &idx)) {
            (State::Done, _) => done.push(format!("  {g} #{} {}", t.id, t.title)),
            (State::Claimed, _) => claimed.push(format!("  {g} #{} {}", t.id, t.title)),
            (State::Open, true) => {
                let needs: Vec<String> = unresolved_blockers(t, tickets, &idx)
                    .iter()
                    .map(|b| format!("#{b}"))
                    .collect();
                blocked.push(format!(
                    "  {g} #{} {}  — needs {}",
                    t.id,
                    t.title,
                    needs.join(", ")
                ));
            }
            (State::Open, false) => frontier.push(format!("  {g} #{} {}", t.id, t.title)),
        }
    }
    let mut lines = Vec::new();
    for (name, group) in [
        ("FRONTIER (ready to claim)", frontier),
        ("CLAIMED", claimed),
        ("BLOCKED", blocked),
        ("DONE", done),
    ] {
        lines.push(format!("{name} — {}", group.len()));
        if group.is_empty() {
            lines.push("  (none)".to_string());
        }
        lines.extend(group);
        lines.push(String::new());
    }
    lines.pop();
    lines
}

// ---------------------------------------------------------------------------
// ratatui TestBackend plumbing: render lines into an in-memory buffer, dump it
// ---------------------------------------------------------------------------

fn render_via_ratatui(lines: &[String]) -> String {
    let w = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as u16;
    let h = lines.len().max(1) as u16;
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
        for gx in 0..buf.area.width {
            row.push_str(buf[(gx, y)].symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------

fn main() {
    let maps: [(&str, Vec<Ticket>); 2] = [
        ("REAL MAP — wayfinder itself (7 tickets, 2 edges)", real_map()),
        ("SYNTHETIC MAP — 25 tickets, 16 blocking edges", synthetic_map()),
    ];
    for (map_name, tickets) in &maps {
        println!("{}", "=".repeat(72));
        println!("{map_name}");
        println!("{LEGEND}");
        println!("{}", "=".repeat(72));
        let forms: [(&str, Vec<String>); 3] = [
            ("Form 1: indented tree (tree edge = first blocker)", render_tree(tickets)),
            ("Form 2: layered left-to-right DAG", render_dag(tickets)),
            ("Form 3: grouped flat list (no graph)", render_grouped(tickets)),
        ];
        for (form_name, lines) in forms {
            println!("\n--- {form_name} ---\n");
            print!("{}", render_via_ratatui(&lines));
        }
        println!();
    }
}
