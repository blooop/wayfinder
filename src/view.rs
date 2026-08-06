//! The body plan (#51, #57): which lines the screen shows, in what order, with
//! what tree furniture — and which of them the cursor can sit on.
//!
//! Everything the body draws is decided here as a [`Plan`] — one list that the
//! cursor and the draw both walk, so the stops the cursor visits are exactly
//! the lines on screen, in on-screen order. Three screens share it:
//!
//! - **Leverage** (the default): per cluster, the takeable tickets (frontier +
//!   claimed) sorted most-open-dependents-first, each with the subtree of open
//!   tickets it unblocks. Done collapses to a [`Item::Group`] line, as do
//!   blocked tickets no subtree reaches; a map with nothing takeable leaves the
//!   body entirely and is only counted ([`Plan::idle_hidden`]).
//! - **Forest** (`tab`): the whole DAG, done dimmed in place. Tree parent =
//!   lowest-numbered in-map blocker; the other in-map blockers annotate the row
//!   (`⤷ also needs #n`).
//! - **Flattened** (a live query): one nucleo-score-ordered flat list across
//!   every cluster in scope — no headers, no tree, every stop at depth 0.
//!   Clearing the query restores the structured screen.
//!
//! Every stop carries its **depth** (#57), because that is what navigation is
//! expressed in: `↑`/`↓` walk siblings at the cursor's own depth, `→` descends
//! into what the cursor is on, `←` comes back out. Depth 0 deliberately spans
//! clusters — the multi-project axis is one list — while deeper levels are
//! bounded by their parent.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::app::Row;
use crate::filter;
use crate::model::{Map, MapId, Status, Ticket};

/// The structural screen `tab` toggles between — the half of the view state
/// that is *stored*. The other half (whether a query is flattening the body)
/// is derived from the query itself in [`Screen`], so the two can never
/// disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// The default: what can be taken now, and what taking it unlocks.
    Leverage,
    /// The whole DAG, done included — the escape hatch when the shape of the
    /// finished work matters.
    Forest,
}

impl Lens {
    pub fn toggled(self) -> Lens {
        match self {
            Lens::Leverage => Lens::Forest,
            Lens::Forest => Lens::Leverage,
        }
    }
}

/// What the body renders this frame. Derived (never stored) from the lens and
/// the query: a live query flattens whichever lens is toggled, and clearing it
/// restores that lens. `Flattened` carries the query, so a flattened screen
/// without one is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen<'a> {
    Structured(Lens),
    Flattened { query: &'a str },
}

/// Which of a cluster's two collapsible groups. Both are the same *kind* of
/// thing — rows the leverage screen holds back behind a count — which is why
/// they share one type and one set of keys rather than the done count being a
/// special case (#57).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKind {
    /// Blocked tickets no rendered subtree reached.
    BlockedDeeper,
    /// Closed tickets.
    Done,
}

/// One collapsible group's identity: which cluster's, and which kind. Durable
/// across a refetch (it names no indices), so it is both the expansion-state
/// key and half of the cursor's anchor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupId {
    pub map: MapId,
    pub kind: GroupKind,
}

/// Which groups the human has opened. Keyed by [`GroupId`], so an expansion
/// survives a refetch, a query, and a lens toggle.
pub type Expanded = BTreeSet<GroupId>;

/// What the cursor is on. Since #57 that is no longer always a ticket: a
/// collapsed group is a stop too, because opening one is an action the cursor
/// has to be able to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Ticket(Row),
    Group(GroupId),
}

/// A stop plus the depth it sits at — everything navigation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAt {
    pub stop: Stop,
    pub depth: usize,
}

/// One line of the body. [`Item::Ticket`] and [`Item::Group`] are cursor
/// stops; headers and spacers are not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A cluster header — the map this and the following lines belong to.
    Header(MapId),
    /// A ticket row.
    Ticket {
        row: Row,
        depth: usize,
        /// Tree branch furniture ("├─", "│ └─", …). Empty at depth 0 and on
        /// every flattened row.
        prefix: String,
        /// Forest only: in-map blockers beyond the primary parent.
        also_needs: Vec<u64>,
    },
    /// A collapsible group of held-back rows, always at depth 0 of its
    /// cluster. Carries what it is holding (`hidden`) and whether it is open,
    /// so the line can say so without consulting anything else.
    Group {
        id: GroupId,
        hidden: usize,
        expanded: bool,
    },
    /// Spacer between clusters.
    Blank,
}

/// The body, planned: the lines in on-screen order, plus how many in-scope
/// maps the leverage screen dropped for having nothing takeable.
#[derive(Debug, Default)]
pub struct Plan {
    pub items: Vec<Item>,
    pub idle_hidden: usize,
}

impl Plan {
    /// The cursor stops, in on-screen order, each with its depth.
    pub fn stops(&self) -> Vec<StopAt> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Ticket { row, depth, .. } => Some(StopAt {
                    stop: Stop::Ticket(row.clone()),
                    depth: *depth,
                }),
                Item::Group { id, .. } => Some(StopAt {
                    stop: Stop::Group(id.clone()),
                    depth: 0,
                }),
                Item::Header(_) | Item::Blank => None,
            })
            .collect()
    }

    /// The ticket rows on screen — what the match count counts. A subset of
    /// [`Plan::stops`]: group lines are stops but not tickets.
    pub fn rows(&self) -> Vec<Row> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Ticket { row, .. } => Some(row.clone()),
                _ => None,
            })
            .collect()
    }
}

/// How much of each row's branch furniture lies on the path from the root down
/// to the cursor: the number of leading two-column units to draw lit rather
/// than dim, indexed by stop.
///
/// The colour follows the **tree**, not the cursor's row. A selection four
/// levels down is connected to its cluster by a run of guides that may pass
/// through many unrelated rows on the way, and it is that whole run which says
/// where you are — lighting only the cursor's own line leaves the eye to guess
/// which of the identical `│` columns above it is the one that leads there.
///
/// What lights is the **chain of branches** leading to the selection: each path
/// node's own `├─`/`└─`, and on the cursor's row its whole indent. Rows the
/// path merely passes on the way — an earlier sibling and its subtree — stay
/// entirely dim, so a lit branch always means "the selection is through here"
/// and never "this row is chosen". The result reads as a staircase from the
/// cluster down to the cursor.
///
/// The lit units of a row are always a leading run, which is why one count per
/// row suffices.
pub fn cursor_path(stops: &[StopAt], cursor: usize) -> Vec<usize> {
    let mut lit = vec![0usize; stops.len()];
    let Some(here) = stops.get(cursor) else {
        return lit;
    };

    // owners[k] is the stop that owns unit position k — the path node at depth
    // k + 1, the deepest of which is the cursor itself.
    let mut owners = vec![cursor; here.depth];
    let mut depth = here.depth;
    for i in (0..cursor).rev() {
        if depth <= 1 {
            break;
        }
        if stops[i].depth == depth - 1 {
            depth -= 1;
            owners[depth - 1] = i;
        }
    }

    for (k, &owner) in owners.iter().enumerate() {
        for (r, at) in stops.iter().enumerate().take(cursor + 1).skip(owner) {
            // Light this unit if it is the path node's own branch, or a
            // continuation guide of it passing through a deeper row.
            if r == owner || at.depth > k + 1 {
                lit[r] = lit[r].max(k + 1);
            }
        }
    }

    // The depth-0 node the path hangs from. A ticket there has no furniture to
    // light, but a *group* has its fold marker, and an open group whose rows
    // the cursor is inside should show it.
    if here.depth > 0 {
        if let Some(root) = (0..cursor).rev().find(|&i| stops[i].depth == 0) {
            lit[root] = lit[root].max(1);
        }
    }
    lit
}

/// Plan the body for the clusters in scope, in their given (render) order.
pub fn plan(clusters: &[(&MapId, &Map)], screen: Screen, expanded: &Expanded) -> Plan {
    match screen {
        Screen::Structured(Lens::Leverage) => leverage(clusters, expanded),
        Screen::Structured(Lens::Forest) => forest(clusters),
        Screen::Flattened { query } => flattened(clusters, query),
    }
}

fn takeable(t: &Ticket) -> bool {
    matches!(t.status, Status::Frontier | Status::Claimed)
}

fn is_open(t: &Ticket) -> bool {
    !matches!(t.status, Status::Done)
}

/// Open direct dependents of `number`, ascending — the subtree children in the
/// leverage view, and the leverage sort key at the roots (done dependents are
/// not leverage: they are already unlocked).
fn open_unblocks(map: &Map, number: u64) -> Vec<u64> {
    map.unblocks(number)
        .into_iter()
        .filter(|&n| map.index_of(n).map(|i| is_open(&map.tickets[i])) == Some(true))
        .collect()
}

fn ticket_item(
    id: &MapId,
    index: usize,
    depth: usize,
    prefix: String,
    also_needs: Vec<u64>,
) -> Item {
    Item::Ticket {
        row: Row {
            map: id.clone(),
            index,
        },
        depth,
        prefix,
        also_needs,
    }
}

/// Push a collapsible group and, when it is open, the rows it holds — as
/// depth-1 children of the group line, so `←` from a held row lands on the
/// group that holds it.
fn push_group(
    items: &mut Vec<Item>,
    id: &MapId,
    kind: GroupKind,
    held: &[usize],
    expanded: &Expanded,
) {
    if held.is_empty() {
        return;
    }
    let group = GroupId {
        map: (*id).clone(),
        kind,
    };
    let is_expanded = expanded.contains(&group);
    items.push(Item::Group {
        id: group,
        hidden: held.len(),
        expanded: is_expanded,
    });
    if !is_expanded {
        return;
    }
    for (i, &index) in held.iter().enumerate() {
        let branch = if i + 1 == held.len() {
            "└─"
        } else {
            "├─"
        };
        items.push(ticket_item(id, index, 1, branch.to_string(), vec![]));
    }
}

fn leverage(clusters: &[(&MapId, &Map)], expanded: &Expanded) -> Plan {
    let mut plan = Plan::default();
    for (id, map) in clusters {
        let mut roots: Vec<&Ticket> = map.tickets.iter().filter(|t| takeable(t)).collect();
        if roots.is_empty() {
            plan.idle_hidden += 1;
            continue;
        }
        roots.sort_by_key(|t| (Reverse(open_unblocks(map, t.number).len()), t.number));

        plan.items.push(Item::Header((*id).clone()));
        let mut reached = BTreeSet::new();
        for root in roots {
            let index = map
                .index_of(root.number)
                .expect("root is one of map.tickets");
            plan.items
                .push(ticket_item(id, index, 0, String::new(), vec![]));
            let mut path = vec![root.number];
            walk_unblocks(
                map,
                id,
                root.number,
                1,
                "",
                &mut path,
                &mut reached,
                &mut plan.items,
            );
        }

        // Blocked tickets no subtree reached — blocked only through issues
        // outside the map (or a blocking cycle): the screen owes a count for
        // what it is not showing, and a way to look.
        let deeper: Vec<usize> = map
            .tickets
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                matches!(t.status, Status::Blocked { .. }) && !reached.contains(&t.number)
            })
            .map(|(i, _)| i)
            .collect();
        push_group(
            &mut plan.items,
            id,
            GroupKind::BlockedDeeper,
            &deeper,
            expanded,
        );

        let done: Vec<usize> = map
            .tickets
            .iter()
            .enumerate()
            .filter(|(_, t)| !is_open(t))
            .map(|(i, _)| i)
            .collect();
        push_group(&mut plan.items, id, GroupKind::Done, &done, expanded);

        plan.items.push(Item::Blank);
    }
    plan.items.pop();
    plan
}

/// Render `number`'s open dependents as a subtree. A dependent that is itself
/// takeable still shows here (what taking the root unlocks includes it) *and*
/// as its own root; a dependent already on the current path is a blocking
/// cycle and is skipped rather than recursed into.
#[allow(clippy::too_many_arguments)]
fn walk_unblocks(
    map: &Map,
    id: &MapId,
    number: u64,
    depth: usize,
    stem: &str,
    path: &mut Vec<u64>,
    reached: &mut BTreeSet<u64>,
    items: &mut Vec<Item>,
) {
    let children: Vec<u64> = open_unblocks(map, number)
        .into_iter()
        .filter(|n| !path.contains(n))
        .collect();
    for (i, &child) in children.iter().enumerate() {
        let last = i + 1 == children.len();
        let index = map
            .index_of(child)
            .expect("dependent is one of map.tickets");
        let (branch, continuation) = if last {
            ("└─", "  ")
        } else {
            ("├─", "│ ")
        };
        items.push(ticket_item(
            id,
            index,
            depth,
            format!("{stem}{branch}"),
            vec![],
        ));
        reached.insert(child);
        path.push(child);
        walk_unblocks(
            map,
            id,
            child,
            depth + 1,
            &format!("{stem}{continuation}"),
            path,
            reached,
            items,
        );
        path.pop();
    }
}

fn forest(clusters: &[(&MapId, &Map)]) -> Plan {
    let mut plan = Plan::default();
    for (id, map) in clusters {
        plan.items.push(Item::Header((*id).clone()));

        // Primary parent = lowest-numbered in-map blocker; everything else on
        // the edge list annotates the row.
        let in_map_blockers = |t: &Ticket| -> Vec<u64> {
            let mut blockers: Vec<u64> = t
                .blocked_by
                .iter()
                .copied()
                .filter(|&b| map.index_of(b).is_some())
                .collect();
            blockers.sort_unstable();
            blockers
        };
        let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        let mut roots: Vec<u64> = vec![];
        for t in &map.tickets {
            match in_map_blockers(t).first() {
                Some(&parent) => children.entry(parent).or_default().push(t.number),
                None => roots.push(t.number),
            }
        }
        roots.sort_unstable();

        let mut visited = BTreeSet::new();
        for &root in &roots {
            walk_forest(
                map,
                id,
                root,
                0,
                "",
                "",
                &children,
                &in_map_blockers,
                &mut visited,
                &mut plan.items,
            );
        }
        // A blocking cycle leaves its members parented to each other and
        // reachable from no root; sweep them in as roots so the forest stays
        // total — every ticket of the map is a row.
        while let Some(orphan) = map
            .tickets
            .iter()
            .map(|t| t.number)
            .find(|n| !visited.contains(n))
        {
            walk_forest(
                map,
                id,
                orphan,
                0,
                "",
                "",
                &children,
                &in_map_blockers,
                &mut visited,
                &mut plan.items,
            );
        }
        plan.items.push(Item::Blank);
    }
    plan.items.pop();
    plan
}

/// Render `number` and its subtree.
///
/// The two prefixes are deliberately separate values. `prefix` is this node's
/// own line furniture, complete; `stem` is the continuation its *children*
/// hang from — the ancestors' vertical bars. Deriving one from the other is
/// what makes deep trees drift a level: a node's line and its children's lines
/// belong to different depths.
#[allow(clippy::too_many_arguments)]
fn walk_forest(
    map: &Map,
    id: &MapId,
    number: u64,
    depth: usize,
    prefix: &str,
    stem: &str,
    children: &BTreeMap<u64, Vec<u64>>,
    in_map_blockers: &dyn Fn(&Ticket) -> Vec<u64>,
    visited: &mut BTreeSet<u64>,
    items: &mut Vec<Item>,
) {
    if !visited.insert(number) {
        return;
    }
    let index = map
        .index_of(number)
        .expect("forest node is one of map.tickets");
    let also_needs: Vec<u64> = in_map_blockers(&map.tickets[index])
        .into_iter()
        .skip(1)
        .collect();
    items.push(ticket_item(
        id,
        index,
        depth,
        prefix.to_string(),
        also_needs,
    ));

    let kids = children.get(&number).cloned().unwrap_or_default();
    for (i, &kid) in kids.iter().enumerate() {
        let last = i + 1 == kids.len();
        // The last child closes its branch and its subtree hangs from blank
        // space; every earlier child keeps the vertical bar running past it.
        let (branch, continuation) = if last {
            ("└─", "  ")
        } else {
            ("├─", "│ ")
        };
        walk_forest(
            map,
            id,
            kid,
            depth + 1,
            &format!("{stem}{branch}"),
            &format!("{stem}{continuation}"),
            children,
            in_map_blockers,
            visited,
            items,
        );
    }
}

fn flattened(clusters: &[(&MapId, &Map)], query: &str) -> Plan {
    let mut scored: Vec<(Reverse<u32>, &MapId, u64, usize)> = Vec::new();
    for (id, map) in clusters {
        for (index, (ticket, score)) in map
            .tickets
            .iter()
            .zip(filter::scores(&map.tickets, query))
            .enumerate()
        {
            if let Some(score) = score {
                scored.push((Reverse(score), id, ticket.number, index));
            }
        }
    }
    // Best score first; ties break to the stable (map, number) screen order.
    scored.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    Plan {
        items: scored
            .into_iter()
            .map(|(_, id, _, index)| ticket_item(id, index, 0, String::new(), vec![]))
            .collect(),
        idle_hidden: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, TicketType};

    fn ticket(number: u64, open: bool, assigned: bool, blocked_by: Vec<u64>) -> Ticket {
        let open_blockers = blocked_by.clone(); // callers pass open blockers in fixtures
        Ticket {
            repo: "blooop/wayfinder".to_string(),
            number,
            title: format!("t{number}"),
            status: classify(open, assigned, if open { open_blockers } else { vec![] }),
            ticket_type: TicketType::Task,
            blocked_by,
            prs: vec![],
        }
    }

    /// Blocked status honestly derived: a blocker is "open" iff it is an open
    /// ticket of this map (fixtures never blocked on out-of-map issues unless
    /// the test says so).
    fn map(tickets: Vec<Ticket>) -> Map {
        let open: BTreeSet<u64> = tickets
            .iter()
            .filter(|t| is_open(t))
            .map(|t| t.number)
            .collect();
        let tickets = tickets
            .into_iter()
            .map(|mut t| {
                if matches!(t.status, Status::Frontier | Status::Blocked { .. }) {
                    let needs: Vec<u64> = t
                        .blocked_by
                        .iter()
                        .copied()
                        .filter(|b| open.contains(b))
                        .collect();
                    t.status = classify(true, false, needs);
                }
                t
            })
            .collect();
        Map {
            title: "Map: fixture".to_string(),
            tickets,
        }
    }

    /// Ticket numbers of the plan's ticket rows, resolved against `map`.
    fn stops(plan: &Plan, m: &Map) -> Vec<u64> {
        plan.rows()
            .iter()
            .map(|r| m.tickets[r.index].number)
            .collect()
    }

    /// Every cursor stop as (what it is, depth) — tickets by number, groups by
    /// kind. This is the navigation surface, so it is what the tests assert on.
    fn nav(plan: &Plan, m: &Map) -> Vec<(String, usize)> {
        plan.stops()
            .into_iter()
            .map(|at| {
                let label = match at.stop {
                    Stop::Ticket(row) => format!("#{}", m.tickets[row.index].number),
                    Stop::Group(g) => format!("{:?}", g.kind),
                };
                (label, at.depth)
            })
            .collect()
    }

    fn id() -> MapId {
        MapId::new("blooop/wayfinder", 47)
    }

    fn nothing() -> Expanded {
        Expanded::new()
    }

    #[test]
    fn leverage_sorts_takeable_by_open_dependents_and_walks_subtrees() {
        // #6 unblocks #7 and #8 (both blocked); #9 unblocks nothing; #2 done.
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6]),
            ticket(8, true, false, vec![6]),
            ticket(9, true, false, vec![]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        // Roots: #6 (2 dependents) before #9 (0); each root's subtree follows it.
        assert_eq!(stops(&plan, &m), vec![6, 7, 8, 9]);
        // The blocked tickets were reached, so only done is held back.
        assert_eq!(
            nav(&plan, &m),
            vec![
                ("#6".to_string(), 0),
                ("#7".to_string(), 1),
                ("#8".to_string(), 1),
                ("#9".to_string(), 0),
                ("Done".to_string(), 0),
            ]
        );
    }

    #[test]
    fn the_takeable_tickets_and_the_groups_are_the_depth_zero_stops() {
        // What `↓` walks: the actionable tickets plus the groups, never the
        // blocked context rows hanging under a root.
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6]),
            ticket(9, true, false, vec![]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        let top: Vec<String> = nav(&plan, &m)
            .into_iter()
            .filter(|(_, depth)| *depth == 0)
            .map(|(label, _)| label)
            .collect();
        assert_eq!(top, vec!["#6", "#9", "Done"]);
    }

    #[test]
    fn an_expanded_group_hangs_its_rows_off_the_group_line() {
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(4, false, false, vec![]),
            ticket(6, true, false, vec![]),
        ]);
        let binding = id();
        let open: Expanded = [GroupId {
            map: binding.clone(),
            kind: GroupKind::Done,
        }]
        .into_iter()
        .collect();

        // Collapsed: the done tickets are not stops at all.
        let collapsed = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(
            nav(&collapsed, &m),
            vec![("#6".to_string(), 0), ("Done".to_string(), 0)]
        );
        assert!(collapsed.items.iter().any(|i| matches!(
            i,
            Item::Group {
                hidden: 2,
                expanded: false,
                ..
            }
        )));

        // Expanded: they become depth-1 children, so `←` from one lands on the
        // group line that holds it.
        let expanded = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage), &open);
        assert_eq!(
            nav(&expanded, &m),
            vec![
                ("#6".to_string(), 0),
                ("Done".to_string(), 0),
                ("#2".to_string(), 1),
                ("#4".to_string(), 1),
            ]
        );
        assert!(expanded
            .items
            .iter()
            .any(|i| matches!(i, Item::Group { expanded: true, .. })));
    }

    #[test]
    fn expansion_is_per_cluster_not_global() {
        // Two clusters both holding done work: opening one must not open the
        // other, which is why the key carries the map.
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(6, true, false, vec![]),
        ]);
        let a = MapId::new("blooop/wayfinder", 47);
        let b = MapId::new("blooop/dotfiles", 4);
        let open: Expanded = [GroupId {
            map: b.clone(),
            kind: GroupKind::Done,
        }]
        .into_iter()
        .collect();
        let plan = plan(
            &[(&b, &m), (&a, &m)],
            Screen::Structured(Lens::Leverage),
            &open,
        );
        let expanded: Vec<bool> = plan
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Group { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(expanded, vec![true, false], "dotfiles open, wayfinder shut");
    }

    #[test]
    fn leverage_holds_back_what_no_subtree_reaches() {
        // #7 blocked only by an out-of-map issue: no root's subtree reaches it,
        // so it is held behind the blocked group rather than silently dropped.
        // Built without the `map` helper — that helper derives blocked status
        // from in-map blockers, and this blocker is deliberately not one.
        let m = Map {
            title: "Map: fixture".to_string(),
            tickets: vec![
                ticket(6, true, false, vec![]),
                ticket(7, true, false, vec![999]),
            ],
        };
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(
            nav(&plan, &m),
            vec![("#6".to_string(), 0), ("BlockedDeeper".to_string(), 0)]
        );
    }

    #[test]
    fn leverage_drops_idle_maps_and_counts_them() {
        let idle = map(vec![ticket(2, false, false, vec![])]);
        let live = map(vec![ticket(6, true, false, vec![])]);
        let idle_id = MapId::new("blooop/dotfiles", 4);
        let live_id = id();
        let plan = plan(
            &[(&idle_id, &idle), (&live_id, &live)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(plan.idle_hidden, 1);
        assert!(
            plan.items
                .iter()
                .all(|i| !matches!(i, Item::Header(h) if h == &idle_id)),
            "the idle cluster left the body: {:?}",
            plan.items
        );
        assert!(plan.items.contains(&Item::Header(live_id)));
    }

    #[test]
    fn leverage_shows_a_claimed_dependent_in_the_subtree_and_as_a_root() {
        // #9 is claimed *and* a dependent of frontier #6 — the accepted
        // prototype shows both: it is takeable, and taking #6 concerns it.
        let mut claimed = ticket(9, true, true, vec![6]);
        claimed.status = Status::Claimed;
        let m = map(vec![ticket(6, true, false, vec![]), claimed]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(stops(&plan, &m), vec![6, 9, 9]);
        // …and only the depth-0 copy is on the `↓` axis.
        assert_eq!(
            nav(&plan, &m),
            vec![
                ("#6".to_string(), 0),
                ("#9".to_string(), 1),
                ("#9".to_string(), 0),
            ]
        );
    }

    #[test]
    fn a_blocking_cycle_does_not_hang_the_leverage_walk() {
        // #7 and #8 block each other, both unblocked-by frontier #6.
        let m = map(vec![
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6, 8]),
            ticket(8, true, false, vec![7]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        // #7 under #6; #8 under #7; the back-edge #8→#7 is skipped, not recursed.
        assert_eq!(stops(&plan, &m), vec![6, 7, 8]);
        assert_eq!(
            nav(&plan, &m),
            vec![
                ("#6".to_string(), 0),
                ("#7".to_string(), 1),
                ("#8".to_string(), 2),
            ]
        );
    }

    #[test]
    fn forest_is_total_with_done_in_place_and_extra_edges_annotated() {
        // #14 needs #6 and #9: parent is #6 (lowest), #9 annotates.
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(6, true, false, vec![]),
            ticket(9, true, true, vec![]),
            ticket(14, true, false, vec![6, 9]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Forest),
            &nothing(),
        );
        assert_eq!(
            stops(&plan, &m),
            vec![2, 6, 14, 9],
            "roots ascend, children under parents"
        );
        let annotated = plan.items.iter().any(|item| {
            matches!(item, Item::Ticket { row, also_needs, .. }
                if m.tickets[row.index].number == 14 && also_needs == &vec![9])
        });
        assert!(annotated, "{:?}", plan.items);
        assert_eq!(plan.idle_hidden, 0, "the forest hides nothing");
        // Nothing is held back, so the forest has no group lines to open.
        assert!(!plan.items.iter().any(|i| matches!(i, Item::Group { .. })));
        // Depth is the real tree depth, so `→` descends the DAG here too.
        assert_eq!(
            nav(&plan, &m),
            vec![
                ("#2".to_string(), 0),
                ("#6".to_string(), 0),
                ("#14".to_string(), 1),
                ("#9".to_string(), 0),
            ]
        );
    }

    #[test]
    fn forest_furniture_stays_aligned_three_levels_deep() {
        // The shape the live wf map exposed: a root with two children, the
        // *first* of which has a child of its own. The grandchild must hang
        // from its parent's running bar (`│ └─`) — a node's own line and its
        // children's lines are different depths, and deriving one from the
        // other drifts by a level.
        let m = map(vec![
            ticket(13, false, false, vec![]),
            ticket(14, false, false, vec![13]),
            ticket(15, false, false, vec![14]),
            ticket(17, false, false, vec![13]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Forest),
            &nothing(),
        );
        let furniture: Vec<(u64, String, usize)> = plan
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Ticket {
                    row, prefix, depth, ..
                } => Some((m.tickets[row.index].number, prefix.clone(), *depth)),
                _ => None,
            })
            .collect();
        assert_eq!(
            furniture,
            vec![
                (13, "".to_string(), 0),
                (14, "├─".to_string(), 1),
                (15, "│ └─".to_string(), 2),
                (17, "└─".to_string(), 1),
            ]
        );
    }

    #[test]
    fn forest_sweeps_a_blocking_cycle_in_rather_than_losing_it() {
        let m = map(vec![
            ticket(7, true, false, vec![8]),
            ticket(8, true, false, vec![7]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Forest),
            &nothing(),
        );
        assert_eq!(
            stops(&plan, &m),
            vec![7, 8],
            "every ticket is a row exactly once"
        );
    }

    /// A cluster shaped so the path to a deep row has to pass *through* an
    /// unrelated sibling subtree — the case that separates "light the tree" from
    /// "light the cursor's row".
    fn forked_map() -> Map {
        map(vec![
            ticket(10, true, false, vec![]),   // root
            ticket(11, true, false, vec![10]), // first child
            ticket(12, true, false, vec![11]), // …with a child of its own
            ticket(13, true, false, vec![10]), // second child: the target
            ticket(14, true, false, vec![13]), // its child
        ])
    }

    #[test]
    fn the_lit_path_runs_from_the_root_down_to_the_cursor() {
        let m = forked_map();
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        // Stops: 0 #10 d0, 1 #11 d1, 2 #12 d2, 3 #13 d1, 4 #14 d2.
        assert_eq!(
            nav(&plan, &m),
            vec![
                ("#10".to_string(), 0),
                ("#11".to_string(), 1),
                ("#12".to_string(), 2),
                ("#13".to_string(), 1),
                ("#14".to_string(), 2),
            ]
        );
        let stops = plan.stops();

        // Cursor on #14, two levels down and past #11's subtree entirely. The
        // chain is #10 → #13 → #14: #13's own branch lights, #14 lights its
        // whole indent, and #11/#12 — merely in the way — stay dark. #10's 1 is
        // the allowance a depth-0 line needs for a group's fold marker; a
        // ticket there has no furniture, so it draws nothing.
        assert_eq!(
            cursor_path(&stops, 4),
            vec![1, 0, 0, 1, 2],
            "#11 and #12 are passed over, not claimed"
        );

        // Cursor on #12: now it is #11's branch that is on the way, and #13/#14
        // (below the cursor) are untouched.
        assert_eq!(cursor_path(&stops, 2), vec![1, 1, 2, 0, 0]);

        // Cursor on a depth-0 root: nothing to trace.
        assert_eq!(cursor_path(&stops, 0), vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_sibling_the_path_passes_is_left_entirely_dim() {
        // The distinction the highlight rests on. #11 is at the same depth as
        // the path node #13 and sits directly above it, so lighting anything on
        // its row would read as a second selection.
        let m = forked_map();
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        let stops = plan.stops();
        let lit = cursor_path(&stops, 4);
        assert_eq!(
            (stops[1].depth, lit[1]),
            (1, 0),
            "#11 is a sibling, not the way"
        );
        assert_eq!((stops[2].depth, lit[2]), (2, 0), "nor is its child #12");
        assert_eq!((stops[3].depth, lit[3]), (1, 1), "#13 is on the chain");
        assert_eq!(
            (stops[4].depth, lit[4]),
            (2, 2),
            "and the cursor lights in full"
        );
    }

    #[test]
    fn the_path_lights_the_group_a_held_row_sits_under() {
        // Selecting a done ticket inside an opened group should light that
        // group's fold marker — the only furniture a depth-0 line has.
        let m = map(vec![
            ticket(2, false, false, vec![]),
            ticket(6, true, false, vec![]),
        ]);
        let binding = id();
        let open: Expanded = [GroupId {
            map: binding.clone(),
            kind: GroupKind::Done,
        }]
        .into_iter()
        .collect();
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage), &open);
        let stops = plan.stops();
        // Stops: 0 #6 d0, 1 Done d0, 2 #2 d1.
        assert_eq!(stops.len(), 3);
        let lit = cursor_path(&stops, 2);
        assert_eq!(lit[1], 1, "the group line the held row hangs from is lit");
        assert_eq!(lit[2], 1, "and the held row's own branch");
        assert_eq!(lit[0], 0, "the unrelated frontier ticket is not");
    }

    #[test]
    fn flattened_orders_by_score_across_clusters_with_no_furniture() {
        let a = map(vec![
            ticket(6, true, false, vec![]),
            ticket(7, false, false, vec![]),
        ]);
        let b = map(vec![ticket(103, true, false, vec![])]);
        let a_id = id();
        let b_id = MapId::new("blooop/dotfiles", 4);
        let plan = plan(
            &[(&b_id, &b), (&a_id, &a)],
            Screen::Flattened { query: "t" },
            &nothing(),
        );
        // Everything matches "t"; no headers, blanks, or groups — rows only,
        // all at depth 0, so `↑`/`↓` walk every hit and `→` has nowhere to go.
        assert!(
            plan.items.iter().all(|i| matches!(i, Item::Ticket { .. })),
            "{:?}",
            plan.items
        );
        assert_eq!(plan.rows().len(), 3, "done tickets stay findable by query");
        assert!(plan.stops().iter().all(|at| at.depth == 0));
        assert_eq!(plan.idle_hidden, 0);
    }

    #[test]
    fn flattened_puts_the_better_match_first_regardless_of_cluster_order() {
        let mut exact = ticket(6, true, false, vec![]);
        exact.title = "breadcrumbs".to_string();
        let mut loose = ticket(103, true, false, vec![]);
        loose.title = "b-r-e-a-d spelled out crumbs".to_string();
        let a = map(vec![loose]);
        let b = map(vec![exact]);
        let a_id = MapId::new("blooop/dotfiles", 4); // sorts before wayfinder
        let b_id = id();
        let plan = plan(
            &[(&a_id, &a), (&b_id, &b)],
            Screen::Flattened { query: "bread" },
            &nothing(),
        );
        let first = plan.rows()[0].clone();
        assert_eq!(first.map, b_id, "score outranks cluster order");
    }
}
