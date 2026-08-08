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
//! - **Sifted** (a live query): whichever of those two trees is toggled, pruned
//!   to the rows that matched. Clearing the query restores it whole.
//!
//! Sifting keeps the structure and drops the rows (the earlier rule flattened
//! both). A cluster keeps its header, a match keeps the branch root it hangs
//! from — the takeable ticket that unlocks it is the one ancestor that says
//! something a match cannot say about itself — and everything between them is
//! chain length: an unmatched link with a single surviving child is elided, and
//! the `⋯` in the next row's furniture (`├⋯`) is what says a level went
//! missing. An unmatched link that *forks* is drawn, because two surviving
//! children need something to hang from. The rows kept only to place other rows
//! are [`Item::Context`] — drawn, never landed on: under a query the only
//! cursor stops are matches, all of them at depth 0, so `↑`/`↓` walk the hits
//! and nothing else, exactly as they did over the flat list.
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
use crate::model::{Map, MapId, RowGlyph, Status, Ticket};

/// The structural screen `tab` toggles between — the half of the view state
/// that is *stored*. The other half (whether a query is sifting the body) is
/// derived from the query itself in [`Screen`], so the two can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// The default: what can be taken now, and what taking it unlocks.
    Leverage,
    /// The whole DAG, done included — the escape hatch when the shape of the
    /// finished work matters.
    Forest,
}

impl Lens {
    #[must_use]
    pub fn toggled(self) -> Lens {
        match self {
            Lens::Leverage => Lens::Forest,
            Lens::Forest => Lens::Leverage,
        }
    }
}

/// What the body renders this frame. Derived (never stored) from the lens and
/// the query: a live query *sifts* whichever lens is toggled — the same tree,
/// pruned to what matched — and clearing it restores that lens whole. `Sifted`
/// carries both halves, so a sifted screen without a query, or one that has
/// forgotten which tree it is pruning, is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen<'a> {
    Structured(Lens),
    Sifted { lens: Lens, query: &'a str },
}

impl Screen<'_> {
    /// The tree this screen draws, whole or pruned. Sifting changes which rows
    /// survive, never which shape they are laid out in.
    #[must_use]
    pub fn lens(self) -> Lens {
        match self {
            Screen::Structured(lens) | Screen::Sifted { lens, .. } => lens,
        }
    }
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
/// has to be able to name — and since #96 so is a cluster header, because a
/// map is a thing you can launch an agent at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// A cluster header: the whole map, launched as one.
    Map(MapId),
    Ticket(Row),
    Group(GroupId),
    /// A focused repo with no open map, by full slug (#114). Nothing to
    /// launch — this is the door its *first* map is charted from, and the one
    /// stop that names a repo rather than something in a map.
    Project(String),
}

/// A stop plus the depth it sits at — everything navigation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAt {
    pub stop: Stop,
    pub depth: usize,
}

/// One line of the body. [`Item::Header`], [`Item::Ticket`] and [`Item::Group`]
/// are cursor stops; spacers and context rows are not — [`Item::stop_at`] is
/// the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A cluster header — the map this and the following lines belong to.
    Header(MapId),
    /// The slim header of a focused repo with no open map (#114): no stage
    /// counts, because there are no stages — just somewhere to stand.
    MaplessHeader(String),
    /// A ticket row.
    Ticket {
        row: Row,
        depth: usize,
        /// Tree branch furniture ("├─", "│ └─", …), empty at the top of a
        /// branch. On a sifted screen it places a match that navigation still
        /// treats as depth 0, and a `⋯` in it marks an elided ancestor.
        prefix: String,
        /// Forest only: in-map blockers beyond the primary parent.
        also_needs: Vec<u64>,
        /// Whether this row heads a branch, and what is in it.
        branch: Branch,
    },
    /// A ticket row drawn only to place the rows beneath it: on a sifted
    /// screen, the branch root a match hangs from, or an unmatched fork two
    /// matches share. Never a cursor stop and never a match, which is why it is
    /// its own line rather than a flag on [`Item::Ticket`] — the count line and
    /// the cursor would both have to remember to check that flag.
    Context { row: Row, prefix: String },
    /// A collapsible group of held-back rows, always at depth 0 of its
    /// cluster. Carries how many rows it stands for (`held`) and its [`Fold`],
    /// so the line can say so without consulting anything else.
    Group {
        id: GroupId,
        held: usize,
        fold: Fold,
    },
    /// Spacer between clusters.
    Blank,
}

impl Item {
    /// The cursor stop this line is, if it is one. The single place that rule
    /// lives: [`Plan::stops`] and the drawn `▶` both read it, so the stop list
    /// and the marker cannot drift apart.
    #[must_use]
    pub fn stop_at(&self) -> Option<StopAt> {
        match self {
            Item::Ticket { row, depth, .. } => Some(StopAt {
                stop: Stop::Ticket(row.clone()),
                depth: *depth,
            }),
            // A header sits at depth 0 alongside the cluster's top-level rows
            // rather than above them, because the depth axis is what `←`/`→`
            // walk and a header is not something you descend *into* — `←` from
            // a top-level row already steps back to it as the previous stop.
            Item::Header(id) => Some(StopAt {
                stop: Stop::Map(id.clone()),
                depth: 0,
            }),
            Item::MaplessHeader(repo) => Some(StopAt {
                stop: Stop::Project(repo.clone()),
                depth: 0,
            }),
            // Nothing to land on. A sifted group leads this arm because it is
            // the narrower pattern: it is a heading the query wrote, not a fold
            // anyone can toggle — clearing the query is what puts the group
            // back — so there is no action for the cursor to name on it.
            Item::Group {
                fold: Fold::Sifted { .. },
                ..
            }
            | Item::Context { .. }
            | Item::Blank => None,
            Item::Group { id, .. } => Some(StopAt {
                stop: Stop::Group(id.clone()),
                depth: 0,
            }),
        }
    }
}

/// Whether a group line is folded — and, **only while it is**, the stage
/// rollup of what it hides (#61): glyph+count pairs in display order, counted
/// once per held node via the rendered tree, so a DAG that reaches a ticket
/// twice cannot count it twice. An open group's rows are right on screen, so
/// a rollup for it is unrepresentable rather than merely unshown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fold {
    Shut {
        rollup: Vec<(RowGlyph, usize)>,
    },
    Open,
    /// A live query is showing `shown` of the group's held rows and holding the
    /// rest back — `shown` is never zero, because a group with nothing matching
    /// leaves the body entirely. The rollup is gone here on purpose: a query
    /// already says what it was looking for, and the group line answers it with
    /// `shown of held` instead.
    Sifted {
        shown: usize,
    },
}

/// Whether a ticket row **heads a branch** — sits at the top of its cluster
/// with a subtree drawn beneath it — and, only when it does, the stage rollup
/// of that subtree (#62): glyph+count pairs in display order.
///
/// One entry per *node*, not per row. This is the place the double-count
/// hazard actually lives: the leverage walk draws a dependent under every root
/// that unblocks it, so a diamond in the DAG — two dependents of one root that
/// both unblock the same ticket — genuinely renders that ticket twice inside
/// the same branch. Counting rows would count it twice; counting the nodes the
/// branch reached counts it once.
///
/// A row inside somebody else's subtree, and a top-level row with nothing
/// beneath it, head no branch — so a rollup on either is unrepresentable
/// rather than merely empty, the same shape [`Fold::Shut`] gives the collapsed
/// groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Branch {
    /// This row heads no branch: either a row inside another row's subtree, or
    /// a top-level row with nothing drawn beneath it.
    Plain,
    /// A branch root, and the stage counts of what it drew beneath itself.
    Root { rollup: Vec<(RowGlyph, usize)> },
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
        self.items.iter().filter_map(Item::stop_at).collect()
    }

    /// The ticket rows on screen — what the match count counts. A subset of
    /// [`Plan::stops`]: group lines are stops but not tickets, and a sifted
    /// screen's context rows are neither.
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
pub fn plan(clusters: &[(&MapId, &Map)], screen: Screen<'_>, expanded: &Expanded) -> Plan {
    let sieve = Sieve::new(clusters, screen);
    // Under a query the cluster order is the query's to decide: the project
    // holding the best hit leads, as it did when the hits were one flat list.
    // Without one the caller's order stands — that is the activity order the
    // multi-project screen is built on.
    let by_score;
    let clusters = match &sieve {
        Sieve::Everything => clusters,
        Sieve::Kept(kept) => {
            by_score = best_first(clusters, kept);
            &by_score
        }
    };
    let mut plan = match screen.lens() {
        Lens::Leverage => leverage(clusters, expanded, &sieve),
        Lens::Forest => forest(clusters, &sieve),
    };
    // Rollups describe the tree that was actually drawn, and a sifted tree
    // draws only the hits: a root there heads its matches, not its subtree, so
    // there is nothing left for a rollup to summarise honestly.
    if !sieve.sifting() {
        attach_rollups(&mut plan.items, clusters);
    }
    plan
}

/// What a live query keeps, and how well each surviving row scored — the whole
/// of what filtering does to a plan. [`Sieve::Everything`] is the no-query
/// case, so the walks below take a sieve unconditionally instead of threading
/// an `Option` no screen can tell them the meaning of.
enum Sieve {
    Everything,
    Kept(BTreeMap<Row, u32>),
}

impl Sieve {
    fn new(clusters: &[(&MapId, &Map)], screen: Screen<'_>) -> Sieve {
        let Screen::Sifted { query, .. } = screen else {
            return Sieve::Everything;
        };
        let mut kept = BTreeMap::new();
        for (id, map) in clusters {
            for (index, score) in filter::scores(&map.tickets, query).into_iter().enumerate() {
                if let Some(score) = score {
                    kept.insert(
                        Row {
                            map: (*id).clone(),
                            index,
                        },
                        score,
                    );
                }
            }
        }
        Sieve::Kept(kept)
    }

    fn sifting(&self) -> bool {
        matches!(self, Sieve::Kept(_))
    }

    /// One cluster's rendered tree, pruned to what the query kept — or handed
    /// straight back when there is no query.
    fn sift(&self, tree: Vec<Item>) -> Vec<Item> {
        match self {
            Sieve::Everything => tree,
            Sieve::Kept(kept) => prune_tree(&tree, kept),
        }
    }
}

/// The clusters ordered by the best score anywhere inside them. Stable, so
/// clusters that match equally well (or not at all — they are about to leave
/// the body) keep the order they came in.
fn best_first<'a>(
    clusters: &[(&'a MapId, &'a Map)],
    kept: &BTreeMap<Row, u32>,
) -> Vec<(&'a MapId, &'a Map)> {
    let mut ordered = clusters.to_vec();
    ordered.sort_by_key(|(id, _)| {
        Reverse(
            kept.iter()
                .filter(|(row, _)| &row.map == *id)
                .map(|(_, score)| *score)
                .max(),
        )
    });
    ordered
}

/// One node of a rendered branch, rebuilt from the flat item list so the prune
/// works on exactly the tree the lens laid out — the same reason
/// [`attach_rollups`] reads the list rather than the DAG. In the leverage view
/// those are different trees: a ticket two roots both unblock is genuinely two
/// nodes here, one under each, and each is pruned on its own.
struct Node {
    row: Row,
    also_needs: Vec<u64>,
    /// This row's own query score — `None` when it is here only for what hangs
    /// beneath it.
    score: Option<u32>,
    kids: Vec<Node>,
}

impl Node {
    /// The best score anywhere in this node's surviving subtree. Sibling order
    /// is decided by it, so the branch holding the top hit leads the screen the
    /// way the flat score-ordered list used to.
    fn best(&self) -> Option<u32> {
        self.kids
            .iter()
            .filter_map(Node::best)
            .chain(self.score)
            .max()
    }

    /// Whether this node earns a line of its own. A match always does; an
    /// unmatched node does only when it forks, because two surviving children
    /// need something to hang from. Everything else is chain length.
    fn drawn(&self) -> bool {
        self.score.is_some() || self.kids.len() > 1
    }
}

/// Rebuild the nodes of a preorder run of ticket rows: a row's subtree is
/// everything after it that is deeper than it is.
fn nodes(run: &[(Row, usize, Vec<u64>)], kept: &BTreeMap<Row, u32>) -> Vec<Node> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < run.len() {
        let (row, depth, also_needs) = &run[i];
        let mut end = i + 1;
        while end < run.len() && run[end].1 > *depth {
            end += 1;
        }
        out.push(Node {
            row: row.clone(),
            also_needs: also_needs.clone(),
            score: kept.get(row).copied(),
            kids: nodes(&run[i + 1..end], kept),
        });
        i = end;
    }
    out
}

/// Drop everything the query did not keep or is not needed to place what it
/// kept, and order what is left best-first at every level.
fn prune(node: Node) -> Option<Node> {
    let mut kids: Vec<Node> = node.kids.into_iter().filter_map(prune).collect();
    if node.score.is_none() && kids.is_empty() {
        return None;
    }
    kids.sort_by_key(|kid| Reverse(kid.best()));
    Some(Node { kids, ..node })
}

/// The children that earn a line, each flagged with whether it was reached
/// through elided ancestors. An unmatched single-child link is not drawn: its
/// child is pulled up into its place, and the `⋯` on that child's link is the
/// only trace left of the levels skipped.
fn drawn_kids(kids: &[Node]) -> Vec<(&Node, bool)> {
    let mut out = Vec::new();
    for kid in kids {
        if kid.drawn() {
            out.push((kid, false));
        } else {
            out.extend(drawn_kids(&kid.kids).into_iter().map(|(n, _)| (n, true)));
        }
    }
    out
}

/// The two-cell link a child hangs from, and the continuation its own children
/// hang from — the two prefixes [`walk_forest`] keeps deliberately separate.
fn link(last: bool, elided: bool) -> (&'static str, &'static str) {
    match (last, elided) {
        (false, false) => ("├─", "│ "),
        (false, true) => ("├⋯", "│ "),
        (true, false) => ("└─", "  "),
        (true, true) => ("└⋯", "  "),
    }
}

/// Draw a surviving node and what survived beneath it. A match is a depth-0
/// stop wherever it sits in the tree — the indentation places it, the depth
/// says only that `↑`/`↓` walk every hit — and everything else is context.
fn emit(node: &Node, prefix: String, stem: &str, out: &mut Vec<Item>) {
    out.push(match node.score {
        Some(_) => Item::Ticket {
            row: node.row.clone(),
            depth: 0,
            prefix,
            also_needs: node.also_needs.clone(),
            branch: Branch::Plain,
        },
        None => Item::Context {
            row: node.row.clone(),
            prefix,
        },
    });
    let kids = drawn_kids(&node.kids);
    for (i, (kid, elided)) in kids.iter().enumerate() {
        let (branch, continuation) = link(i + 1 == kids.len(), *elided);
        emit(
            kid,
            format!("{stem}{branch}"),
            &format!("{stem}{continuation}"),
            out,
        );
    }
}

/// One cluster's rendered tree, pruned to the matches and the rows that place
/// them. The branch roots always survive; see the module header for why.
fn prune_tree(tree: &[Item], kept: &BTreeMap<Row, u32>) -> Vec<Item> {
    // The trees handed here are built by the lens walks, which push nothing but
    // ticket rows; headers, groups and spacers are added around them afterwards.
    let run: Vec<(Row, usize, Vec<u64>)> = tree
        .iter()
        .filter_map(|item| match item {
            Item::Ticket {
                row,
                depth,
                also_needs,
                ..
            } => Some((row.clone(), *depth, also_needs.clone())),
            _ => None,
        })
        .collect();
    let mut roots: Vec<Node> = nodes(&run, kept).into_iter().filter_map(prune).collect();
    roots.sort_by_key(|root| Reverse(root.best()));
    let mut out = Vec::new();
    for root in &roots {
        emit(root, String::new(), "", &mut out);
    }
    out
}

/// Fill in each branch root's rollup, read off the rows the plan actually laid
/// out.
///
/// A pass over the finished list rather than something the walks carry down:
/// "what is beneath this row" is a fact about the *rendered* tree, and the
/// rendered tree is exactly this list — so every screen gets the same answer
/// from the same code, and there is no second notion of a subtree to drift
/// from the one on screen. A top-level ticket row opens a branch and every
/// deeper ticket row until the next top-level one is in it, each node kept
/// once however many times the walk drew it. A header, a spacer, or a group
/// line closes the open branch: the rows a group holds hang from the group,
/// not from the last ticket above it.
fn attach_rollups(items: &mut [Item], clusters: &[(&MapId, &Map)]) {
    let maps: BTreeMap<&MapId, &Map> = clusters.iter().copied().collect();
    let mut branches: Vec<(usize, Vec<Row>)> = Vec::new();
    let mut open: Option<(usize, Vec<Row>)> = None;
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Ticket { depth: 0, .. } => {
                branches.extend(open.replace((i, Vec::new())));
            }
            Item::Ticket { row, .. } => {
                if let Some((_, beneath)) = &mut open {
                    if !beneath.contains(row) {
                        beneath.push(row.clone());
                    }
                }
            }
            // `Item::Context` cannot appear here: rollups are attached only to
            // the unsifted screens, and only a sift produces context rows.
            Item::Header(_)
            | Item::MaplessHeader(_)
            | Item::Group { .. }
            | Item::Context { .. }
            | Item::Blank => {
                branches.extend(open.take());
            }
        }
    }
    branches.extend(open);

    for (i, beneath) in branches {
        if beneath.is_empty() {
            continue;
        }
        let rollup = RowGlyph::tally(beneath.iter().map(|row| &maps[&row.map].tickets[row.index]));
        if let Item::Ticket { branch, .. } = &mut items[i] {
            *branch = Branch::Root { rollup };
        }
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
        .filter(|&n| map.index_of(n).is_some_and(|i| is_open(&map.tickets[i])))
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
        // Filled in by `attach_rollups` once the whole list exists: whether a
        // row heads a branch is a property of the rows that follow it.
        branch: Branch::Plain,
    }
}

/// Push a collapsible group and, when it is open, the rows it holds — as
/// depth-1 children of the group line, so `←` from a held row lands on the
/// group that holds it. A shut group carries the stage rollup of its held
/// rows instead: each held node contributes its glyph once.
///
/// A live query reaches inside: the group renders [`Fold::Sifted`] holding only
/// its matches (at depth 0, like every other match) and says how many of how
/// many that is, so done work stays findable by typing without the screen
/// having to pretend the rest of the group is not there. A group with nothing
/// matching does not render at all.
fn push_group(
    items: &mut Vec<Item>,
    id: &MapId,
    map: &Map,
    kind: GroupKind,
    held: &[usize],
    expanded: &Expanded,
    sieve: &Sieve,
) {
    if held.is_empty() {
        return;
    }
    let group = GroupId {
        map: (*id).clone(),
        kind,
    };
    let (fold, shown, depth) = match sieve {
        Sieve::Kept(kept) => {
            let mut matched: Vec<(usize, u32)> = held
                .iter()
                .filter_map(|&index| {
                    let row = Row {
                        map: (*id).clone(),
                        index,
                    };
                    kept.get(&row).map(|&score| (index, score))
                })
                .collect();
            if matched.is_empty() {
                return;
            }
            // Stable, so held rows that score alike keep the map's own order.
            matched.sort_by_key(|&(_, score)| Reverse(score));
            (
                Fold::Sifted {
                    shown: matched.len(),
                },
                matched.into_iter().map(|(index, _)| index).collect(),
                0,
            )
        }
        Sieve::Everything if expanded.contains(&group) => (Fold::Open, held.to_vec(), 1),
        Sieve::Everything => (
            Fold::Shut {
                rollup: RowGlyph::tally(held.iter().map(|&index| &map.tickets[index])),
            },
            vec![],
            1,
        ),
    };
    items.push(Item::Group {
        id: group,
        held: held.len(),
        fold,
    });
    for (i, &index) in shown.iter().enumerate() {
        let (branch, _) = link(i + 1 == shown.len(), false);
        items.push(ticket_item(id, index, depth, branch.to_string(), vec![]));
    }
}

fn leverage(clusters: &[(&MapId, &Map)], expanded: &Expanded, sieve: &Sieve) -> Plan {
    let mut plan = Plan::default();
    for (id, map) in clusters {
        let mut roots: Vec<&Ticket> = map.tickets.iter().filter(|t| takeable(t)).collect();
        // A map with nothing takeable leaves the leverage body and is only
        // counted. Not while a query is live, though: its done and blocked work
        // is exactly as findable by typing as anyone else's, so the cluster is
        // built anyway and the sieve decides whether any of it survives.
        if roots.is_empty() && !sieve.sifting() {
            plan.idle_hidden += 1;
            continue;
        }
        roots.sort_by_key(|t| (Reverse(open_unblocks(map, t.number).len()), t.number));

        let mut tree = Vec::new();
        let mut reached = BTreeSet::new();
        for root in roots {
            let index = map
                .index_of(root.number)
                .expect("root is one of map.tickets");
            tree.push(ticket_item(id, index, 0, String::new(), vec![]));
            let mut path = vec![root.number];
            walk_unblocks(
                map,
                id,
                root.number,
                1,
                "",
                &mut path,
                &mut reached,
                &mut tree,
            );
        }
        let mut body = sieve.sift(tree);

        // Blocked tickets no subtree reached — blocked only through issues
        // outside the map (or a blocking cycle): the screen owes a count for
        // what it is not showing, and a way to look. Reached-ness is read off
        // the whole walk, before the sieve, so a query cannot push a ticket
        // into this group by pruning the branch that reached it.
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
            &mut body,
            id,
            map,
            GroupKind::BlockedDeeper,
            &deeper,
            expanded,
            sieve,
        );

        let done: Vec<usize> = map
            .tickets
            .iter()
            .enumerate()
            .filter(|(_, t)| !is_open(t))
            .map(|(i, _)| i)
            .collect();
        push_group(&mut body, id, map, GroupKind::Done, &done, expanded, sieve);

        // Only a sift can empty a cluster — a map with a takeable root always
        // has a row — and a cluster a query emptied leaves the body header and
        // all, the way filtering is expected to work.
        if body.is_empty() {
            continue;
        }
        plan.items.push(Item::Header((*id).clone()));
        plan.items.append(&mut body);
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
        let index = map
            .index_of(child)
            .expect("dependent is one of map.tickets");
        let (branch, continuation) = link(i + 1 == children.len(), false);
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

fn forest(clusters: &[(&MapId, &Map)], sieve: &Sieve) -> Plan {
    let mut plan = Plan::default();
    for (id, map) in clusters {
        let mut tree = Vec::new();

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
                &mut tree,
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
                &mut tree,
            );
        }
        let mut body = sieve.sift(tree);
        // The forest is total, so an empty cluster here is always a query's
        // doing: nothing in this map matched, and the cluster goes with it.
        if body.is_empty() {
            continue;
        }
        plan.items.push(Item::Header((*id).clone()));
        plan.items.append(&mut body);
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
        // The last child closes its branch and its subtree hangs from blank
        // space; every earlier child keeps the vertical bar running past it.
        let (branch, continuation) = link(i + 1 == kids.len(), false);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Checks, PrLink, PrStatus, Review, Stage, TicketType};

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
            last_activity: None,
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

    /// Every cursor stop as (what it is, depth) — tickets by number, maps by
    /// `map #n`, groups by kind. This is the navigation surface, so it is what
    /// the tests assert on.
    /// The cluster header's stop, which every plan below opens with since #96.
    /// Named once so the assertions stay about the rows under it.
    fn header() -> (String, usize) {
        ("map #47".to_string(), 0)
    }

    fn nav(plan: &Plan, m: &Map) -> Vec<(String, usize)> {
        plan.stops()
            .into_iter()
            .map(|at| {
                let label = match at.stop {
                    Stop::Map(id) => format!("map #{}", id.number),
                    Stop::Ticket(row) => format!("#{}", m.tickets[row.index].number),
                    Stop::Group(g) => format!("{:?}", g.kind),
                    Stop::Project(repo) => format!("project {repo}"),
                };
                (label, at.depth)
            })
            .collect()
    }

    /// Every top-level ticket row as (number, what it heads) — the branch
    /// roots and what each says about the subtree drawn beneath it.
    fn roots(plan: &Plan, m: &Map) -> Vec<(u64, Branch)> {
        plan.items
            .iter()
            .filter_map(|item| match item {
                Item::Ticket {
                    row,
                    depth: 0,
                    branch,
                    ..
                } => Some((m.tickets[row.index].number, branch.clone())),
                _ => None,
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
                header(),
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
        assert_eq!(top, vec!["map #47", "#6", "#9", "Done"]);
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
            vec![header(), ("#6".to_string(), 0), ("Done".to_string(), 0)]
        );
        assert!(collapsed.items.iter().any(|i| matches!(
            i,
            Item::Group {
                held: 2,
                fold: Fold::Shut { .. },
                ..
            }
        )));

        // Expanded: they become depth-1 children, so `←` from one lands on the
        // group line that holds it.
        let expanded = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage), &open);
        assert_eq!(
            nav(&expanded, &m),
            vec![
                header(),
                ("#6".to_string(), 0),
                ("Done".to_string(), 0),
                ("#2".to_string(), 1),
                ("#4".to_string(), 1),
            ]
        );
        assert!(expanded.items.iter().any(|i| matches!(
            i,
            Item::Group {
                fold: Fold::Open,
                ..
            }
        )));
    }

    #[test]
    fn every_cluster_opens_with_its_header_as_a_stop() {
        // #96: a map is a thing you can launch an agent at, so the cursor has
        // to be able to name it. One stop per cluster, at the front of it and
        // at depth 0 — context rows and spacers stay unreachable.
        let m = map(vec![ticket(6, true, false, vec![])]);
        let other = MapId::new("blooop/dotfiles", 4);
        let binding = id();
        let plan = plan(
            &[(&binding, &m), (&other, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        let headers: Vec<MapId> = plan
            .stops()
            .into_iter()
            .filter_map(|at| match at.stop {
                Stop::Map(id) => Some(id),
                Stop::Ticket(_) | Stop::Group(_) | Stop::Project(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![binding, other],
            "one per cluster, in render order"
        );
        assert_eq!(
            plan.stops().first().map(|at| at.depth),
            Some(0),
            "a header sits alongside its top-level rows, not above them"
        );
    }

    #[test]
    fn a_shut_group_carries_a_stage_rollup_of_what_it_holds() {
        // The done group holds #2 (plain done) and #4 — closed, but its PR is
        // still open and approved, so it reads in-review: a closed ticket
        // whose PR never landed is exactly what the rollup exists to surface.
        let mut with_pr = ticket(4, false, false, vec![]);
        with_pr.prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }];
        let m = map(vec![
            ticket(2, false, false, vec![]),
            with_pr,
            ticket(6, true, false, vec![2]),
            ticket(9, true, false, vec![2]),
        ]);
        let binding = id();
        let shut = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        let fold = shut
            .items
            .iter()
            .find_map(|i| match i {
                Item::Group { held, fold, .. } => Some((*held, fold.clone())),
                _ => None,
            })
            .expect("the done group");
        assert_eq!(
            fold,
            (
                2,
                Fold::Shut {
                    rollup: vec![
                        (RowGlyph::Stage(Stage::InReview), 1),
                        (RowGlyph::Stage(Stage::Done), 1),
                    ]
                }
            ),
            "glyph+count pairs in display order, once per held node"
        );

        // Open, the rows are right there — the rollup only exists while shut,
        // so a rollup on an expanded row is unrepresentable, not just unshown.
        let open: Expanded = [GroupId {
            map: binding.clone(),
            kind: GroupKind::Done,
        }]
        .into_iter()
        .collect();
        let opened = plan(&[(&binding, &m)], Screen::Structured(Lens::Leverage), &open);
        assert!(opened.items.iter().any(|i| matches!(
            i,
            Item::Group {
                fold: Fold::Open,
                ..
            }
        )));
    }

    #[test]
    fn a_node_the_rendered_tree_reaches_twice_is_counted_once() {
        // The DAG's diamond: #6 and #9 are both takeable and both unblock #7,
        // so the leverage view genuinely renders #7 *twice* — once under each
        // root. That is the hazard the rollup rule exists for ("counted once
        // per node via the rendered tree", #61): counts must come from the
        // set of nodes a group holds, not from the rows on screen.
        let m = map(vec![
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6, 9]),
            ticket(9, true, false, vec![]),
            ticket(2, false, false, vec![]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        // The premise, pinned: without it the rest of this test proves nothing.
        assert_eq!(
            stops(&plan, &m).into_iter().filter(|&n| n == 7).count(),
            2,
            "#7 hangs under both #6 and #9"
        );

        // Every shut group's counts sum to exactly what it holds — one entry
        // per held node, and #7, rendered twice but held by nobody, is in no
        // rollup at all.
        let shut: Vec<(usize, Vec<(RowGlyph, usize)>)> = plan
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Group {
                    held,
                    fold: Fold::Shut { rollup },
                    ..
                } => Some((*held, rollup.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            shut.len(),
            1,
            "only the done group holds anything: {shut:?}"
        );
        for (hidden, rollup) in shut {
            assert_eq!(
                rollup.iter().map(|(_, n)| n).sum::<usize>(),
                hidden,
                "rollup totals what the group holds, not what was drawn"
            );
        }
    }

    #[test]
    fn the_blocked_deeper_rollup_reads_blocked_not_stage() {
        // A held-back blocked ticket rolls up as ⊘ — its stage is unactionable
        // and the override carries into the counts (#62).
        let m = Map {
            title: "Map: fixture".to_string(),
            last_activity: None,
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
        let fold = plan
            .items
            .iter()
            .find_map(|i| match i {
                Item::Group { fold, .. } => Some(fold.clone()),
                _ => None,
            })
            .expect("the blocked-deeper group");
        assert_eq!(
            fold,
            Fold::Shut {
                rollup: vec![(RowGlyph::Blocked, 1)]
            }
        );
    }

    #[test]
    fn a_branch_root_carries_the_stage_rollup_of_its_subtree() {
        // #6 unblocks blocked #7 and claimed #9; #20 is takeable and unblocks
        // nothing. So #6 heads a branch and says what is in it, while #20 —
        // and #9 in its own turn as a root — head nothing and say nothing.
        let m = map(vec![
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6]),
            ticket(9, true, true, vec![6]),
            ticket(20, true, false, vec![]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(
            roots(&plan, &m),
            vec![
                (
                    6,
                    Branch::Root {
                        rollup: vec![
                            (RowGlyph::Stage(Stage::Building), 1),
                            (RowGlyph::Blocked, 1),
                        ]
                    }
                ),
                // Claimed #9 is drawn under #6 *and* as its own root. As a
                // root it heads nothing, so it carries no rollup — the copy
                // inside #6's branch is what #6 counted.
                (9, Branch::Plain),
                (20, Branch::Plain),
            ]
        );
    }

    #[test]
    fn a_node_the_branch_renders_twice_is_counted_once() {
        // The diamond, inside one root's branch: #6 unblocks #7 and #8, and
        // both of those unblock #10 — so the leverage walk genuinely draws #10
        // twice under #6. Counting the rows drawn would say four; the branch
        // holds three nodes.
        let m = map(vec![
            ticket(6, true, false, vec![]),
            ticket(7, true, false, vec![6]),
            ticket(8, true, false, vec![6]),
            ticket(10, true, false, vec![7, 8]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        // The premise, pinned: without it the assertion below proves nothing.
        assert_eq!(
            stops(&plan, &m),
            vec![6, 7, 10, 8, 10],
            "#10 hangs under both #7 and #8"
        );
        assert_eq!(
            roots(&plan, &m),
            vec![(
                6,
                Branch::Root {
                    rollup: vec![(RowGlyph::Blocked, 3)]
                }
            )],
            "three nodes beneath #6, not the four rows drawn for them"
        );
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
                Item::Group { fold, .. } => Some(matches!(fold, Fold::Open)),
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
            last_activity: None,
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
            vec![
                header(),
                ("#6".to_string(), 0),
                ("BlockedDeeper".to_string(), 0)
            ]
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
                header(),
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
                header(),
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
                header(),
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
                (13, String::new(), 0),
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

    /// A ticket with a title of its own — the sifting tests are about which
    /// rows a query keeps, so they need titles a query can tell apart.
    fn titled(number: u64, title: &str, open: bool, blocked_by: Vec<u64>) -> Ticket {
        Ticket {
            title: title.to_string(),
            ..ticket(number, open, false, blocked_by)
        }
    }

    /// The body as one string per line, furniture included: this is a test
    /// about the *shape* a query leaves behind, so the shape is what it reads.
    fn shape(plan: &Plan, m: &Map) -> Vec<String> {
        let number = |row: &Row| m.tickets[row.index].number;
        plan.items
            .iter()
            .map(|item| match item {
                Item::Header(id) => format!("▌ {}", id.short_repo()),
                Item::MaplessHeader(repo) => format!("▌ {repo} no map"),
                Item::Ticket { row, prefix, .. } => format!("{prefix}#{}", number(row)),
                Item::Context { row, prefix } => format!("{prefix}#{} dim", number(row)),
                Item::Group {
                    id,
                    held,
                    fold: Fold::Sifted { shown },
                } => format!("{:?} {shown}/{held}", id.kind),
                Item::Group { id, held, .. } => format!("{:?} {held}", id.kind),
                Item::Blank => String::new(),
            })
            .collect()
    }

    fn sifted(query: &str) -> Screen<'_> {
        Screen::Sifted {
            lens: Lens::Leverage,
            query,
        }
    }

    #[test]
    fn a_query_keeps_the_cluster_and_the_root_its_match_hangs_from() {
        // #14 matches, nothing else does. The header stays (which project this
        // is), the takeable #6 stays as context (what unlocks the match), and
        // #7 — neither a match nor on the way to one — goes.
        let m = map(vec![
            titled(6, "root", true, vec![]),
            titled(7, "sibling", true, vec![6]),
            titled(14, "alpha", true, vec![6]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(shape(&plan, &m), vec!["▌ wayfinder", "#6 dim", "└─#14"]);
        // The context row is drawn but not landed on: `↑`/`↓` walk the hits,
        // all of them depth 0, exactly as they did over the flat list.
        assert_eq!(nav(&plan, &m), vec![header(), ("#14".to_string(), 0)]);
        assert_eq!(plan.rows().len(), 1, "context rows are not matches");
    }

    #[test]
    fn an_unmatched_link_with_one_surviving_child_elides() {
        // #7 is on the way to the match and nothing else: it is chain length,
        // and the `⋯` on #14's link is all that is left of it.
        let m = map(vec![
            titled(6, "root", true, vec![]),
            titled(7, "link", true, vec![6]),
            titled(14, "alpha", true, vec![7]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(shape(&plan, &m), vec!["▌ wayfinder", "#6 dim", "└⋯#14"]);
    }

    #[test]
    fn an_unmatched_link_that_forks_is_drawn() {
        // Two matches hang off #7, so #7 earns its line: without it the two
        // would read as siblings under #6, which is a different tree.
        let m = map(vec![
            titled(6, "root", true, vec![]),
            titled(7, "link", true, vec![6]),
            titled(12, "alpha", true, vec![7]),
            titled(13, "alpha", true, vec![7]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(
            shape(&plan, &m),
            vec!["▌ wayfinder", "#6 dim", "└─#7 dim", "  ├─#12", "  └─#13"]
        );
    }

    #[test]
    fn the_branch_holding_the_best_match_leads_its_cluster() {
        // #20's title is the query exactly; #14 spells it out scattered. The
        // branch that holds the better hit comes first, so a query still walks
        // best-first even though the rows are back in a tree.
        let m = map(vec![
            titled(6, "root", true, vec![]),
            titled(14, "a-l-p-h-a spelled out", true, vec![6]),
            titled(9, "other root", true, vec![]),
            titled(20, "alpha", true, vec![9]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(
            shape(&plan, &m),
            vec!["▌ wayfinder", "#9 dim", "└─#20", "#6 dim", "└─#14"]
        );
    }

    #[test]
    fn the_cluster_holding_the_best_match_leads_the_screen() {
        // What the flat list used to give for free: score outranks the cluster
        // order the multi-project screen is otherwise built on.
        let loose = map(vec![titled(103, "a-l-p-h-a spelled out", true, vec![])]);
        let exact = map(vec![titled(6, "alpha", true, vec![])]);
        let loose_id = MapId::new("blooop/dotfiles", 4);
        let exact_id = id();
        let plan = plan(
            &[(&loose_id, &loose), (&exact_id, &exact)],
            sifted("alpha"),
            &nothing(),
        );
        assert_eq!(
            plan.items.first(),
            Some(&Item::Header(exact_id)),
            "{:?}",
            plan.items
        );
    }

    #[test]
    fn a_cluster_with_nothing_matching_leaves_the_body() {
        let hit = map(vec![titled(6, "alpha", true, vec![])]);
        let miss = map(vec![titled(103, "nothing like it", true, vec![])]);
        let hit_id = id();
        let miss_id = MapId::new("blooop/dotfiles", 4);
        let plan = plan(
            &[(&miss_id, &miss), (&hit_id, &hit)],
            sifted("alpha"),
            &nothing(),
        );
        assert_eq!(
            plan.items,
            vec![
                Item::Header(hit_id.clone()),
                Item::Ticket {
                    row: Row {
                        map: hit_id,
                        index: 0
                    },
                    depth: 0,
                    prefix: String::new(),
                    also_needs: vec![],
                    branch: Branch::Plain,
                },
            ],
            "no header, no spacer, no trace of the cluster that missed"
        );
    }

    #[test]
    fn a_query_opens_a_group_onto_its_matches_and_says_how_many_of_how_many() {
        // Done work stays findable by typing (it did when a query flattened
        // the body, and it has to still). The group line stays too, because
        // showing one of five done tickets and saying "done" would be a lie.
        let m = map(vec![
            titled(2, "alpha", false, vec![]),
            titled(4, "unrelated", false, vec![]),
            titled(6, "root", true, vec![]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(shape(&plan, &m), vec!["▌ wayfinder", "Done 1/2", "└─#2"]);
        // The group is a heading here, not a fold: only the match is a stop.
        assert_eq!(nav(&plan, &m), vec![header(), ("#2".to_string(), 0)]);
    }

    #[test]
    fn a_query_reaches_into_a_map_the_leverage_screen_drops() {
        // A map with nothing takeable is not on the leverage screen at all —
        // but its finished work is exactly as findable by typing as anyone
        // else's, so the sieve gets to look before the map is dropped.
        let m = map(vec![titled(2, "alpha", false, vec![])]);
        let binding = id();
        let idle = plan(
            &[(&binding, &m)],
            Screen::Structured(Lens::Leverage),
            &nothing(),
        );
        assert_eq!(idle.idle_hidden, 1, "the premise: nothing takeable here");
        assert!(idle.items.is_empty());

        let found = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert_eq!(shape(&found, &m), vec!["▌ wayfinder", "Done 1/1", "└─#2"]);
        assert_eq!(
            found.idle_hidden, 0,
            "the map is on screen, so there is nothing to say it is hidden"
        );
    }

    #[test]
    fn sifting_the_forest_prunes_the_forest() {
        // The lens the query sifts is whichever one is toggled: on the forest
        // the tree parent is the blocker, and done rows are in place rather
        // than behind a group — so the same match hangs from a different row.
        let m = map(vec![
            titled(2, "finished", false, vec![]),
            titled(6, "root", true, vec![]),
            titled(14, "alpha", true, vec![6]),
        ]);
        let binding = id();
        let plan = plan(
            &[(&binding, &m)],
            Screen::Sifted {
                lens: Lens::Forest,
                query: "alpha",
            },
            &nothing(),
        );
        assert_eq!(shape(&plan, &m), vec!["▌ wayfinder", "#6 dim", "└─#14"]);
    }

    #[test]
    fn a_sifted_screen_carries_no_rollups() {
        // A rollup says what a row heads. On a sifted screen a row heads its
        // matches, not its subtree, so the honest rollup is no rollup: #6
        // unblocks two tickets here and only one of them survived the query.
        let m = map(vec![
            titled(6, "root", true, vec![]),
            titled(7, "sibling", true, vec![6]),
            titled(14, "alpha", true, vec![6]),
        ]);
        let binding = id();
        let plan = plan(&[(&binding, &m)], sifted("alpha"), &nothing());
        assert!(
            plan.items.iter().all(|item| !matches!(
                item,
                Item::Ticket {
                    branch: Branch::Root { .. },
                    ..
                }
            )),
            "{:?}",
            plan.items
        );
    }
}
