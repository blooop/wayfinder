//! The body plan (#51): which rows the screen shows, in what order, with what
//! tree furniture.
//!
//! Everything the body draws is decided here as a [`Plan`] — one list that the
//! cursor and the draw both walk, so the rows the cursor stops on are exactly
//! the ticket rows on screen, in on-screen order. Three screens share it:
//!
//! - **Leverage** (the default): per cluster, the takeable tickets (frontier +
//!   claimed) sorted most-open-dependents-first, each with the subtree of open
//!   tickets it unblocks. Done collapses to a count; blocked tickets no subtree
//!   reaches collapse to a count; a map with nothing takeable leaves the body
//!   entirely and is only counted ([`Plan::idle_hidden`]).
//! - **Forest** (`tab`): the whole DAG, done dimmed in place. Tree parent =
//!   lowest-numbered in-map blocker; the other in-map blockers annotate the row
//!   (`⤷ also needs #n`).
//! - **Flattened** (a live query): one nucleo-score-ordered flat list across
//!   every cluster in scope — no headers, no tree. Clearing the query restores
//!   the structured screen.

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

/// One line of the body. Only [`Item::Ticket`] is a cursor stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A cluster header — the map this and the following rows belong to.
    Header(MapId),
    /// A ticket row.
    Ticket {
        row: Row,
        /// Tree branch furniture ("├─", "│ └─", …). Empty at roots and on
        /// every flattened row.
        prefix: String,
        /// Forest only: in-map blockers beyond the primary parent.
        also_needs: Vec<u64>,
    },
    /// Leverage: `⊘ N blocked deeper down` — blocked tickets no rendered
    /// subtree reached, so they are on screen only as this count.
    BlockedDeeper(usize),
    /// Leverage: `● N done (hidden)`.
    DoneHidden(usize),
    /// Spacer between clusters.
    Blank,
}

/// The body, planned: the items in on-screen order, plus how many in-scope
/// maps the leverage screen dropped for having nothing takeable.
#[derive(Debug, Default)]
pub struct Plan {
    pub items: Vec<Item>,
    pub idle_hidden: usize,
}

impl Plan {
    /// The cursor stops, in on-screen order.
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

/// Plan the body for the clusters in scope, in their given (render) order.
pub fn plan(clusters: &[(&MapId, &Map)], screen: Screen) -> Plan {
    match screen {
        Screen::Structured(Lens::Leverage) => leverage(clusters),
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

fn ticket_item(id: &MapId, index: usize, prefix: String, also_needs: Vec<u64>) -> Item {
    Item::Ticket {
        row: Row {
            map: id.clone(),
            index,
        },
        prefix,
        also_needs,
    }
}

fn leverage(clusters: &[(&MapId, &Map)]) -> Plan {
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
                .push(ticket_item(id, index, String::new(), vec![]));
            let mut path = vec![root.number];
            walk_unblocks(
                map,
                id,
                root.number,
                "",
                &mut path,
                &mut reached,
                &mut plan.items,
            );
        }

        // Blocked tickets no subtree reached — blocked only through issues
        // outside the map (or a blocking cycle): the screen owes a count for
        // what it is not showing.
        let deeper = map
            .tickets
            .iter()
            .filter(|t| matches!(t.status, Status::Blocked { .. }) && !reached.contains(&t.number))
            .count();
        if deeper > 0 {
            plan.items.push(Item::BlockedDeeper(deeper));
        }
        let done = map.tickets.iter().filter(|t| !is_open(t)).count();
        if done > 0 {
            plan.items.push(Item::DoneHidden(done));
        }
        plan.items.push(Item::Blank);
    }
    plan.items.pop();
    plan
}

/// Render `number`'s open dependents as a subtree. A dependent that is itself
/// takeable still shows here (what taking the root unlocks includes it) *and*
/// as its own root; a dependent already on the current path is a blocking
/// cycle and is skipped rather than recursed into.
fn walk_unblocks(
    map: &Map,
    id: &MapId,
    number: u64,
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
        let branch = if last { "└─" } else { "├─" };
        items.push(ticket_item(id, index, format!("{stem}{branch}"), vec![]));
        reached.insert(child);
        let next = if last {
            format!("{stem}  ")
        } else {
            format!("{stem}│ ")
        };
        path.push(child);
        walk_unblocks(map, id, child, &next, path, reached, items);
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
                "",
                None,
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
                "",
                None,
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

#[allow(clippy::too_many_arguments)]
fn walk_forest(
    map: &Map,
    id: &MapId,
    number: u64,
    stem: &str,
    branch: Option<&str>,
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
    let prefix = branch.map(|b| format!("{stem}{b}")).unwrap_or_default();
    items.push(ticket_item(id, index, prefix, also_needs));

    let kids = children.get(&number).cloned().unwrap_or_default();
    for (i, &kid) in kids.iter().enumerate() {
        let last = i + 1 == kids.len();
        let next = match branch {
            None => stem.to_string(),
            Some(_) if last => format!("{stem}  "),
            Some(_) => format!("{stem}│ "),
        };
        let kid_branch = if last { "└─" } else { "├─" };
        walk_forest(
            map,
            id,
            kid,
            &next,
            Some(kid_branch),
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
            .map(|(_, id, _, index)| ticket_item(id, index, String::new(), vec![]))
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

    /// Ticket numbers of the plan's cursor stops, resolved against `map`.
    fn stops(plan: &Plan, m: &Map) -> Vec<u64> {
        plan.rows()
            .iter()
            .map(|r| m.tickets[r.index].number)
            .collect()
    }

    fn id() -> MapId {
        MapId::new("blooop/wayfinder", 47)
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
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage));
        // Roots: #6 (2 dependents) before #9 (0); each root's subtree follows it.
        assert_eq!(stops(&plan, &m), vec![6, 7, 8, 9]);
        // The blocked tickets were reached, and the done one collapsed.
        assert!(!plan.items.contains(&Item::BlockedDeeper(2)));
        assert!(plan.items.contains(&Item::DoneHidden(1)));
    }

    #[test]
    fn leverage_counts_what_no_subtree_reaches() {
        // #7 blocked only by an out-of-map issue: no root's subtree reaches it.
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
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage));
        assert_eq!(stops(&plan, &m), vec![6]);
        assert!(
            plan.items.contains(&Item::BlockedDeeper(1)),
            "{:?}",
            plan.items
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
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage));
        assert_eq!(stops(&plan, &m), vec![6, 9, 9]);
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
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage));
        // #7 under #6; #8 under #7; the back-edge #8→#7 is skipped, not recursed.
        assert_eq!(stops(&plan, &m), vec![6, 7, 8]);
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
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Forest));
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
    }

    #[test]
    fn forest_sweeps_a_blocking_cycle_in_rather_than_losing_it() {
        let m = map(vec![
            ticket(7, true, false, vec![8]),
            ticket(8, true, false, vec![7]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], Screen::Structured(Lens::Forest));
        assert_eq!(
            stops(&plan, &m),
            vec![7, 8],
            "every ticket is a row exactly once"
        );
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
        );
        // Everything matches "t"; no headers, blanks, or counts — rows only.
        assert!(
            plan.items.iter().all(|i| matches!(i, Item::Ticket { .. })),
            "{:?}",
            plan.items
        );
        assert_eq!(plan.rows().len(), 3, "done tickets stay findable by query");
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
        );
        let first = plan.rows()[0].clone();
        assert_eq!(first.map, b_id, "score outranks cluster order");
    }
}
