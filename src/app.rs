//! Main-screen state and the keybindings (#14).
//!
//! [`App`] owns everything the screen needs between keypresses: the clusters
//! (one per open map, #50), the fuzzy query, the cursor over *visible* rows,
//! the project scope, and a one-shot notice line. Key handling returns an
//! [`Outcome`] so the binary owns the side effects: the app decides *what* to
//! launch and `main` is the only thing that may act on it, because acting on
//! it means giving the terminal back and never coming here again (#34).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::launch::{self, Agent, Candidate, Launch, LaunchMode, MapRef, Route, Staged, Targets};
use crate::liveness::Liveness;
use crate::model::{stage, Activity, Map, MapId, MapSet, Status, Ticket};
use crate::projects::{self, Checkout, Resume, Session};
use crate::reclaim::Reclaimable;
use crate::refresh::Startup;
use crate::view::{self, Expanded, GroupId, Lens, Plan, Screen, Stop, StopAt};

/// Which of the two screens is up — the whole of `wf`'s navigation.
///
/// This replaces the old `Scope { All, Project }`, and the difference is not
/// cosmetic. A scope was a *filter* on one screen: `Scope::All` rendered every
/// project's clusters at once and `ctrl-f` narrowed them to one, so both states
/// ran the same cluster-rendering code and the repo you were standing in was
/// something the body had to be searched for. A level is the screen itself. The
/// project list has no cluster code in it at all, and [`Level::Project`] always
/// names a repo — so the map-less repo, which used to be an exception carved
/// into the widened screen, is just a project whose list of maps is empty.
///
/// Two arms, and no `All` among them: seeing every project means seeing the
/// *projects*, not every project's tickets poured into one tree. That is the
/// reversal — one screen per project, reached by picking one — and it is what
/// retires `ctrl-f`, `ctrl-g` and `ctrl-p` together, since focusing, widening
/// and finding a project are all now just moving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    /// The project list: every registered repo, most recently used first.
    /// Where `wf` opens when it was not run inside a checkout.
    Projects,
    /// One project: its maps, under the project's own row. Where `wf` opens
    /// when it *was* run inside a checkout, and where selecting a project from
    /// the list arrives — the same screen either way.
    Project { repo: String },
}

/// What the event loop should do after a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    /// Run this ticket's agent. The last thing `wf` ever does: the loop
    /// returns, the terminal is restored, and the process becomes the agent.
    ///
    /// Boxed because it is by far the largest thing an outcome can carry — a
    /// launch holds the whole snapshot it hands its agent (#124) — and every
    /// other outcome is a keystroke's worth of nothing. One allocation on the
    /// last keypress of the session buys a small `Outcome` for all the rest.
    Launch(Box<Launch>),
}

/// A modal layer over the main screen: the staged second step of a launch
/// (#62), or the which-checkout prompt. Either owns every key while up, so no
/// typing leaks into the query behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// The launch picker — `enter` on a launchable node staged this launch, and
    /// the overlay collects its agent, a complete [`Candidate`], and any text
    /// needed to fill that candidate. The candidate carries its resolved route,
    /// so the picker shows exactly what `enter` will run. An unlaunchable node
    /// never reaches this state: [`launch::Launchable`] refuses it first.
    ///
    /// The staged launch is index-free ([`Staged`]) for the same reason the
    /// picker's candidates are complete `Launch`es: a background map arrival
    /// swaps the clusters underneath an open overlay, and a positional [`Row`]
    /// held across that would name a different ticket, or none at all.
    PickLaunch {
        staged: Staged,
        agent: Agent,
        /// The picked row — one of [`Staged::candidates`], which is the only
        /// list the arrows walk, so a candidate foreign to the staged stop
        /// (creation on a ticket) is never held here (#114).
        candidate: Candidate,
        steer: String,
    },
    /// Candidates are complete launches, so the pick cannot produce an
    /// inconsistent one.
    PickCheckout {
        launches: Vec<Launch>,
        cursor: usize,
    },
}

/// The row after `candidate` in the launch picker, wrapping.
///
/// Walked over [`Staged::candidates`] rather than [`Mode::all`] since #114:
/// the list is the staged stop's own, so a header's creation rows are reached
/// by the same arrows that walk the modes, and a ticket's picker cannot step
/// onto a row it does not draw.
fn next_candidate(staged: &Staged, candidate: Candidate) -> Candidate {
    stepped(staged, candidate, 1)
}

/// The row before it. Backwards is a forward step of `len - 1`, so there is no
/// signed arithmetic and no underflow to reason about at index 0.
fn previous_candidate(staged: &Staged, candidate: Candidate) -> Candidate {
    let len = staged.candidates().len();
    stepped(staged, candidate, len - 1)
}

/// Step `delta` places along the staged stop's candidates, wrapping. Takes a
/// distance rather than a key, so which key means which direction stays in the
/// key handler where the rest of the bindings are.
///
/// # Panics
///
/// Never: [`Staged::candidates`] is never empty — every stop offers rows, its
/// launch rows or a project row's creation rows — so the modulo below is
/// never by zero.
fn stepped(staged: &Staged, candidate: Candidate, delta: usize) -> Candidate {
    let candidates = staged.candidates();
    let at = candidates.iter().position(|c| *c == candidate).unwrap_or(0);
    candidates[(at + delta) % candidates.len()]
}

/// Move horizontally between the two launch agents. The agent is the execution
/// environment, unlike [`Mode`] which changes the workflow route, so the two
/// axes deliberately have different keys in the same picker.
fn next_agent(agent: Agent) -> Agent {
    agent.other()
}

/// One on-screen row: which map's cluster it is in, and the ticket's position
/// within that cluster. The map half matters (#50): two maps of one repo may
/// list the same ticket, and those are two distinct rows.
///
/// **Positional, and only valid against the clusters it was read from** — an
/// index into a `Vec` that the next fetch replaces. [`RowKey`] is the durable
/// half. They are separate named types rather than two same-shaped tuples
/// because that is the distinction that matters and the one a `.0`/`.1` at a
/// call site cannot show.
/// `Ord` is (map, index) — screen order within a cluster, and the key the
/// sifted view looks a row's query score up by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Row {
    pub map: MapId,
    /// Index into that cluster's `tickets`.
    pub index: usize,
}

/// A row's stable identity across refreshes: the map and the ticket *number* —
/// what the cursor anchors to when clusters are swapped underneath it. Unlike
/// [`Row`] this survives a refetch, which is the whole reason both exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowKey {
    pub map: MapId,
    pub ticket: u64,
}

/// A *stop's* stable identity — [`RowKey`] widened to cover the other thing the
/// cursor can be on since #57. A group needs no widening of its own: a
/// [`GroupId`] already names a map and a kind rather than any index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopKey {
    Map(MapId),
    /// A ticket row, plus **which drawing of it** — the leverage lens
    /// deliberately draws one ticket under every root that unblocks it, so on
    /// a DAG diamond the [`RowKey`] alone names two stops. Without the
    /// occurrence, pinning "the row they chose" resolved to the *first*
    /// matching drawing, teleporting a cursor parked on the second one on
    /// every refetch (#188).
    Ticket(RowKey, usize),
    Group(GroupId),
    /// A whole project, by repo slug — already index-free, like the
    /// map and group keys.
    Project(String),
}

impl StopKey {
    /// The first drawing of the same stop — the identity to fall back to when
    /// the exact drawing left the screen but the ticket did not (a lens
    /// toggle, a root that closed). Only tickets are ever drawn twice, so the
    /// other arms are already their own first occurrence.
    fn first_occurrence(&self) -> StopKey {
        match self {
            StopKey::Ticket(row, _) => StopKey::Ticket(row.clone(), 0),
            other => other.clone(),
        }
    }
}

/// Where the cursor is, and — the part that matters — **whether anyone put it
/// there** (#88).
///
/// Two facts used to be one `usize`, and index `0` had to stand for both of
/// them: the position a fresh screen starts at, and a row the human deliberately
/// picked. [`App::replace_clusters`] cannot serve both. A chosen row is pinned by
/// identity, so a map arriving above it must never teleport the selection
/// (#50/#57); a starting position has no identity to pin, and pinning it anyway
/// is what dragged the cursor down to the second cluster as the first-arrived map
/// was outranked by a fresher one.
///
/// A sum type rather than a flag beside the index, so the state "untouched, and
/// also remembering a position" — the one that made a default indistinguishable
/// from a choice — cannot be written down. [`Cursor::Untouched`] carries no
/// index because it does not have one: it *means* the top of the list, and the
/// top is re-read from whatever list is on screen now.
///
/// The chosen arm holds a position rather than a [`StopKey`] because the key is
/// re-derived from the live clusters at the moment they are swapped, and the
/// index is what carries `preserve_cursor`'s fallback for a stop that vanished
/// entirely. Holding both would let the two disagree, which is a worse defect
/// than the one this fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// Never moved. Always means "the first stop on screen", whatever the screen
    /// becomes — taken literally: the first stop drawn, not the first *takeable*
    /// one, so an untouched cursor never skips past a cluster whose top rows
    /// happen to be blocked or done.
    Untouched,
    /// Put here deliberately, at this position in [`App::stops`]. Identity is
    /// preserved across a reorder or a refetch.
    Chosen(usize),
}

/// How a chosen cursor rides out a cluster swap: two different holds, so two
/// arms (#148). **Identity** is the normal one — the stop under the cursor,
/// carried with the old position as the fallback should it vanish from the
/// new order. **Position** is what remains when the cursor is chosen over an
/// *empty* screen — a swap can empty the list under a choice — where there is
/// no stop to name. A `None` inside a tuple used to say that by omission,
/// which let "pinned by identity" and "pinned by position" share one shape
/// whose halves were told apart by reading the inner option at the use site
/// rather than by matching.
enum Pinned {
    /// Follow this stop wherever the new order puts it; fall back to the old
    /// position, clamped, if it is gone.
    Identity(StopKey, usize),
    /// Nothing to follow: hold the bare position, clamped.
    Position(usize),
}

impl Pinned {
    /// Where the hold lands once `new_order` is the order on screen.
    /// Each arm answers for itself, so the two shapes stay two shapes all
    /// the way down to the one call that needs them to be one.
    fn resolve(&self, new_order: &[StopKey]) -> usize {
        match self {
            Pinned::Identity(key, fallback) => {
                // The exact drawing when the new order still has it; else the
                // first drawing of the same ticket — a swap that removed the
                // root a duplicate hung under took the drawing, not the
                // ticket, and the cursor's promise is the ticket. Only when
                // both are gone does the positional fallback apply.
                let key = if new_order.contains(key) {
                    key.clone()
                } else {
                    key.first_occurrence()
                };
                crate::refresh::preserve_cursor(Some(&key), *fallback, new_order)
            }
            Pinned::Position(fallback) => {
                crate::refresh::preserve_cursor(None, *fallback, new_order)
            }
        }
    }
}

#[derive(Debug)]
pub struct App {
    /// The clusters on screen: every open map that has arrived, keyed by id.
    /// Render order is *not* this map's key order — it is decided by
    /// [`App::scoped_clusters`], which leads on activity.
    pub clusters: BTreeMap<MapId, Map>,
    /// The maps believed open — the cached seed until the search answers, the
    /// search's answer afterwards. This is the set the loaders are reconciled
    /// against, and with one load per run the search's answer is the last word
    /// on it.
    pub open_maps: MapSet,
    pub query: String,
    /// Which screen is up: the project list, or one project's own.
    pub level: Level,
    /// One-shot status message shown on the count line; cleared on the next
    /// keypress.
    pub notice: Option<String>,
    /// Launch input from the projects cache (#15 handoff): which checkouts
    /// exist on this machine.
    pub checkouts: Vec<Checkout>,
    /// The other half of that handoff (#35): the conversations previous
    /// launches left here, at most one per node.
    pub sessions: Vec<Session>,
    /// Maps whose last fetch failed — **state, not a message.**
    ///
    /// A failure has to be drawn on every frame, and the one-shot `notice` is
    /// cleared by the very next keypress. Nothing polls and nothing asks again,
    /// so a failed fetch is the final word on that map for the rest of the run:
    /// with only a notice, one keystroke turns "GitHub is down" into a screen
    /// that says *no projects — run wf inside a checkout to register it*, which
    /// is the exact lie [`crate::refresh::Startup`] exists to prevent for the
    /// still-loading case.
    ///
    /// A set of [`MapId`]s rather than a flag, because the flag it replaces
    /// could not say *which* map, and a partial failure — four clusters on
    /// screen, one missing — is the case that hides best.
    pub failed: BTreeSet<MapId>,
    /// How much of the initial load has landed (#27). The screen is drawn
    /// before any of it, so this is what stops an empty list from reading as
    /// "no tickets" while the fetch is still out.
    pub startup: Startup,
    /// What a `wf reap` would claim, once the background reading lands (#137)
    /// — `None` until then, and `None` forever if the reading failed or found
    /// nothing, which are deliberately the same thing.
    ///
    /// **State, not a notice**, for the reason [`App::failed`] is: the reading
    /// arrives once and nothing asks again, so a message the next keypress
    /// cleared would be gone before it was read. Nothing in the picker acts on
    /// it — it is a sentence naming a command a person may choose to type.
    pub reclaimable: Option<Reclaimable>,
    /// What is running on this machine, and what stopped without finishing —
    /// the other half of the same background reading (#137's survey, read for
    /// a second question).
    ///
    /// Not an `Option`: an empty [`Liveness`] and one that has not arrived draw
    /// exactly the same nothing, and every reader of it asks about one node at
    /// a time. A second layer of "have we looked yet" would be a distinction no
    /// caller can act on.
    pub liveness: Liveness,
    pub overlay: Overlay,
    /// The structural screen `tab` toggles (#51). Only the lens is stored;
    /// whether the body is currently *flattened* is derived from the query in
    /// [`App::screen`], so the two can never disagree.
    lens: Lens,
    /// Which collapsible groups the human has opened (#57). Keyed by
    /// [`GroupId`], so an expansion survives a refetch, a query and a lens
    /// toggle — it is a choice about a *map*, not about a frame.
    expanded: Expanded,
    cursor: Cursor,
    /// What staging already did about each workspace this session
    /// ([`launch::prewarm`]): the first enter fires `dl <ws> up` in the
    /// background so the container is building while the human types steer
    /// text, and this is what keeps re-staging the same node from firing a
    /// second one. Session-scoped on purpose — after `wf` execs away, the
    /// workspace's own state answers.
    ///
    /// A map rather than a set because the launch that follows hands `dl` the
    /// instant the warm-up fired (#160), and "it fired" without "when" is not
    /// something a timing reader can weigh a launch against.
    prewarmed: BTreeMap<String, Prewarmed>,
}

/// What staging did about one workspace's container — the record a launch of
/// that workspace then reads.
///
/// A sum rather than an `Option<SystemTime>` beside a claim flag, because the
/// two states answer different questions and both are ordinary: a node was
/// warmed at an instant, or it was looked at and found to have no container to
/// warm. Absence from the map is the third state — never staged this session —
/// and needs no variant of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prewarmed {
    /// `dl <workspace> up` went out at this instant.
    At(SystemTime),
    /// Nothing was fired and nothing would be: this node's launch plans no
    /// container, so there is no warm-up to stamp.
    Nothing,
}

impl App {
    /// An app over clusters already in hand — so nothing is being waited on.
    pub fn new(clusters: BTreeMap<MapId, Map>) -> Self {
        Self {
            clusters,
            open_maps: MapSet::new(),
            query: String::new(),
            level: Level::Projects,
            notice: None,
            checkouts: Vec::new(),
            sessions: Vec::new(),
            failed: BTreeSet::new(),
            startup: Startup::loaded(),
            reclaimable: None,
            liveness: Liveness::default(),
            overlay: Overlay::None,
            lens: Lens::Leverage,
            expanded: Expanded::new(),
            cursor: Cursor::Untouched,
            prewarmed: BTreeMap::new(),
        }
    }

    /// An app with no data yet, drawn while the load is still in flight (#27).
    /// The distinction from [`App::new`] is the whole point: same empty list,
    /// two different meanings, told apart by [`App::startup`] rather than left
    /// to the reader.
    pub fn empty() -> Self {
        Self {
            startup: Startup::default(),
            ..Self::new(BTreeMap::new())
        }
    }

    /// Attach the launch input: the cached checkouts (the candidate trees an
    /// agent could run in). Which map a ticket belongs to is no longer a
    /// lookup — every row sits in a cluster that *is* its map (#50).
    #[must_use]
    pub fn with_checkouts(mut self, checkouts: Vec<Checkout>) -> Self {
        self.checkouts = checkouts;
        self
    }

    /// Attach the conversations previous launches left on this machine (#35) —
    /// what puts a resume row under the picker's cursor, and a `⏎` beside the
    /// rows that have one.
    ///
    /// Read from the same cache as the checkouts and at the same moment, which
    /// is what keeps this a **frame-zero** fact: the badge is drawn on the
    /// first paint, before the map search has answered, because it was never
    /// the tracker's to answer. Nothing here asks `dl` anything.
    #[must_use]
    pub fn with_sessions(mut self, sessions: Vec<Session>) -> Self {
        self.sessions = sessions;
        self
    }

    /// The conversation a previous launch of this node left, if any.
    pub fn resume(&self, repo: &str, number: u64) -> Option<&Resume> {
        self.sessions
            .iter()
            .find(|s| s.repo == repo && s.number == number)
            .map(|s| &s.resume)
    }

    /// Attach `staged`'s resume, if its node has one. One helper rather than
    /// the same lookup at the two staging sites, so a ticket and a map cannot
    /// end up disagreeing about what counts as resumable.
    fn resumable(&self, staged: Staged, number: u64) -> Staged {
        match self.resume(&staged.repo, number) {
            Some(resume) => staged.with_resume(resume.clone()),
            None => staged,
        }
    }

    /// The repo whose screen is up, or `None` on the project list.
    pub fn current_repo(&self) -> Option<&str> {
        match &self.level {
            Level::Projects => None,
            Level::Project { repo } => Some(repo),
        }
    }

    /// The registered repos, most recently used first — the project list's
    /// body, and the order the cursor walks.
    pub fn projects(&self) -> Vec<String> {
        projects::mru_repos(&self.checkouts)
    }

    /// Enter a project: its screen, from the top. The cursor is deliberately
    /// *not* carried over — the stop it was on was a project row on another
    /// screen, and the new screen's own first stop is this project's row,
    /// which is where an untouched cursor already means.
    pub fn enter(&mut self, repo: &str) {
        self.level = Level::Project {
            repo: repo.to_string(),
        };
        self.query.clear();
        self.cursor = Cursor::Untouched;
    }

    /// Back out to the project list, with the cursor on the project just left
    /// — so `←` then `↓` walks to the next project rather than starting over
    /// at the top of the list.
    fn leave(&mut self) {
        let Level::Project { repo } = &self.level else {
            return;
        };
        let key = StopKey::Project(repo.clone());
        self.level = Level::Projects;
        self.query.clear();
        self.point_at(&key);
    }

    fn in_scope(&self, id: &MapId) -> bool {
        self.current_repo() == Some(id.repo.as_str())
    }

    /// The clusters currently in scope, in render order: **work before
    /// history, most recently active first.**
    ///
    /// Three keys, in this order:
    ///
    /// 1. Whether the map still has open work ([`Map::has_open_work`]). A
    ///    finished map is history, and history belongs at the bottom however
    ///    recently it was finished — otherwise a map completed this morning
    ///    outranks every live one, which is the exact inversion this order
    ///    exists to fix.
    /// 2. Last activity, newest first. `None` (a timestamp that did not parse)
    ///    sorts after every known one rather than being guessed into place.
    /// 3. The [`MapId`] — (repo, number), the stable tie-break, so equal
    ///    activity never renders in an arbitrary order that shifts between
    ///    frames.
    pub fn scoped_clusters(&self) -> Vec<(&MapId, &Map)> {
        // `!has_open_work` so `false` (live) sorts before `true` (finished), and
        // `Reverse` so the newest activity leads — with `None` last, since
        // `None < Some` reversed puts the unknown stamps at the end. Declared
        // before the first statement: an item after one is `clippy::pedantic`'s
        // `items_after_statements`, which CI escalates to an error.
        fn key<'a>(
            (id, map): &(&'a MapId, &'a Map),
        ) -> (bool, Reverse<Option<Activity>>, &'a MapId) {
            (!map.has_open_work(), Reverse(map.last_activity), *id)
        }
        let mut clusters: Vec<(&MapId, &Map)> = self
            .clusters
            .iter()
            .filter(|(id, _)| self.in_scope(id))
            .collect();
        clusters.sort_by(|a, b| key(a).cmp(&key(b)));
        clusters
    }

    /// Rows currently in scope (before the fuzzy query), cluster-major.
    pub fn scoped(&self) -> Vec<Row> {
        self.scoped_clusters()
            .into_iter()
            .flat_map(|(id, map)| {
                (0..map.tickets.len()).map(|index| Row {
                    map: id.clone(),
                    index,
                })
            })
            .collect()
    }

    /// What the body renders this frame (#51): the toggled lens, sifted down to
    /// the matches while a query is live. Derived, never stored.
    pub fn screen(&self) -> Screen<'_> {
        if self.query.is_empty() {
            Screen::Structured(self.lens)
        } else {
            Screen::Sifted {
                lens: self.lens,
                query: &self.query,
            }
        }
    }

    /// The body, planned: every line the screen shows, in on-screen order —
    /// what the draw walks and what the cursor navigates, so the two can never
    /// disagree about order.
    ///
    /// The two levels are two plans, and neither can draw the other's lines:
    /// the project list has no cluster in it, and a project screen never lists
    /// a repo that is not its own.
    pub fn plan(&self) -> Plan {
        match &self.level {
            Level::Projects => view::projects(
                &self.projects(),
                &self.clusters,
                self.startup.is_loaded(),
                &self.query,
            ),
            // The project's own row leads whenever there is no query — it is a
            // place to stand, not a report on what was found, so it is there
            // whether the repo has ten maps, none, or none *yet*. That is the
            // whole of what the map-less empty-state door used to be, minus
            // the door.
            //
            // A live query drops it, and that is not a detail: an untouched
            // cursor means the first stop, so a project row surviving a sift
            // would take `enter` on a freshly typed query away from the best
            // match and give it to *create a new task* — the one place where
            // the creating default this row exists for is the wrong one. The
            // row is also not a match, and a sifted body is matches and the
            // structure that places them.
            Level::Project { repo } => view::plan(
                &self.scoped_clusters(),
                self.screen(),
                &self.expanded,
                self.query
                    .is_empty()
                    .then(|| view::ProjectRow::new(repo, &self.clusters, self.startup.is_loaded())),
            ),
        }
    }

    /// Every cursor stop with its depth, in on-screen order (#57). The cursor
    /// indexes this list; headers and spacers are never stops, but group lines
    /// are — opening one is an action, so it needs naming.
    pub fn stops(&self) -> Vec<StopAt> {
        self.plan().stops()
    }

    /// Ticket rows on screen. A subset of [`App::stops`].
    pub fn visible(&self) -> Vec<Row> {
        self.plan().rows()
    }

    /// The count line's `shown/total` — **of whatever this screen is a list
    /// of**, which is the only reading that stays true across the two levels.
    ///
    /// A project list counting *tickets* would read `0/0` on a screen with
    /// nine projects on it: a fetch that has not landed and a repo with
    /// nothing open would look identical to the screen being empty, and the
    /// number a query narrows would not be the number the query narrowed.
    ///
    /// On a project's screen both halves count **tickets**, so the numerator
    /// dedups the drawn rows: the leverage lens deliberately draws a ticket
    /// under *every* root that unblocks it, and counting rows there let a
    /// diamond in the DAG claim more tickets shown than the map holds — the
    /// same nodes-not-rows discipline the rollups already keep.
    pub fn counts(&self) -> (usize, usize) {
        match self.level {
            Level::Projects => (
                self.stops().len(),
                projects::mru_repos(&self.checkouts).len(),
            ),
            Level::Project { .. } => {
                let shown: BTreeSet<(MapId, usize)> = self
                    .visible()
                    .into_iter()
                    .map(|row| (row.map, row.index))
                    .collect();
                (shown.len(), self.scoped().len())
            }
        }
    }

    /// The ticket a row names.
    pub fn ticket(&self, row: &Row) -> &Ticket {
        &self.clusters[&row.map].tickets[row.index]
    }

    /// A row's durable identity — the anchor a refetch preserves.
    pub fn row_key(&self, row: &Row) -> RowKey {
        RowKey {
            map: row.map.clone(),
            ticket: self.ticket(row).number,
        }
    }

    /// Every stop's durable identity, in on-screen order: a ticket by
    /// (map, number) plus which drawing of it this is, a map or a group by its
    /// own id — which already name no indices.
    ///
    /// Computed over the whole list rather than stop-by-stop because the
    /// occurrence *is* list context: a key derived from one stop alone cannot
    /// know it is the second drawing of a diamond-duplicated ticket, and a key
    /// that cannot say so pins two stops to one identity (#188).
    fn stop_keys(&self) -> Vec<StopKey> {
        let mut keys: Vec<StopKey> = Vec::new();
        for at in self.stops() {
            let key = match &at.stop {
                Stop::Map(id) => StopKey::Map(id.clone()),
                Stop::Ticket(row) => {
                    let row_key = self.row_key(row);
                    let occurrence = keys
                        .iter()
                        .filter(|k| matches!(k, StopKey::Ticket(prev, _) if *prev == row_key))
                        .count();
                    StopKey::Ticket(row_key, occurrence)
                }
                Stop::Group(id) => StopKey::Group(id.clone()),
                Stop::Project(repo) => StopKey::Project(repo.clone()),
            };
            keys.push(key);
        }
        keys
    }

    /// Cursor position clamped into the stop list. An untouched cursor is not a
    /// remembered position but a *rule* — the top of the list — so it is
    /// answered from the stops on screen now rather than from anything stored.
    ///
    /// "The top" skips cluster headers, and only those. #88 settled that the
    /// default is the first stop **literally**, not the first *takeable* one —
    /// a blocked or claimed row still gets the cursor. A header is not a row
    /// competing for that place, though: it is the container the rows sit in,
    /// and defaulting onto one would make `enter` on a freshly filtered screen
    /// launch the whole map rather than the row that was filtered to. So the
    /// #96 stops are reachable by `↑` from the row below them, and never by
    /// arriving.
    pub fn cursor_pos(&self) -> usize {
        match self.cursor {
            Cursor::Untouched => self
                .stops()
                .iter()
                .position(|at| !matches!(at.stop, Stop::Map(_)))
                .unwrap_or(0),
            Cursor::Chosen(pos) => pos.min(self.stops().len().saturating_sub(1)),
        }
    }

    /// What the cursor is on, if anything is on screen.
    pub fn cursor_stop(&self) -> Option<Stop> {
        self.stops()
            .get(self.cursor_pos())
            .map(|at| at.stop.clone())
    }

    /// The row under the cursor — `None` when the cursor is on a header or a
    /// group line, which is exactly why they are different types.
    pub fn cursor_row(&self) -> Option<Row> {
        match self.cursor_stop() {
            Some(Stop::Ticket(row)) => Some(row),
            Some(Stop::Map(_) | Stop::Group(_) | Stop::Project(_)) | None => None,
        }
    }

    /// The ticket under the cursor, if the cursor is on one.
    pub fn cursor_ticket(&self) -> Option<&Ticket> {
        self.cursor_row().map(|row| {
            let map: &Map = &self.clusters[&row.map];
            &map.tickets[row.index]
        })
    }

    /// The cursor's stable identity — read off the whole key list, since which
    /// drawing the cursor is on is a fact about the list, not the stop.
    fn cursor_key(&self) -> Option<StopKey> {
        self.stop_keys().into_iter().nth(self.cursor_pos())
    }

    /// The stop to hold on to while the list is rebuilt underneath the cursor —
    /// a scope change, a lens toggle. `None` for an untouched cursor, which has
    /// no stop to hold: it means the top of whatever list results, and anchoring
    /// it to the row that merely happens to be first would turn a default into a
    /// choice the human never made.
    fn cursor_anchor(&self) -> Option<StopKey> {
        match self.cursor {
            Cursor::Untouched => None,
            Cursor::Chosen(_) => self.cursor_key(),
        }
    }

    /// Point the cursor at a specific stop — if that stop is on the screen the
    /// rebuild produced.
    ///
    /// When it is not, the cursor goes back to [`Cursor::Untouched`], because an
    /// anchor that vanished is the *absence* of a choice and not a choice of the
    /// first row. The forest lens emits no group lines at all, so `tab` from a
    /// `Done` line deletes the stop being held; writing down `Chosen(0)` there
    /// would promote a default into a choice through a key that was never about the
    /// selection, and the next map to sort above would then carry the cursor off
    /// the top — #88 exactly. `move_sibling` already answers the same fact the same
    /// way for an empty list.
    ///
    /// Hence the exhaustive match rather than `unwrap_or(0)`: the `None` is the
    /// case that matters here, so it is named instead of being given a value.
    fn point_at(&mut self, key: &StopKey) {
        let keys = self.stop_keys();
        // The exact drawing first; failing that, any drawing of the same
        // ticket — a lens toggle deletes the duplicates the leverage screen
        // drew, and losing the drawing must not mean losing the ticket.
        let first = key.first_occurrence();
        let found = keys
            .iter()
            .position(|k| k == key)
            .or_else(|| keys.iter().position(|k| *k == first));
        self.cursor = match found {
            Some(pos) => Cursor::Chosen(pos),
            None => Cursor::Untouched,
        };
    }

    /// `↑`/`↓`: the next stop at the cursor's own depth, else simply the next
    /// stop (#57).
    ///
    /// Preferring *siblings* is what makes the default screen a pick list: at
    /// depth 0 the stops are the takeable tickets and the group lines, so `↓`
    /// moves between things you can act on and steps over the blocked context
    /// hanging beneath them. The scan gives up at a shallower stop, because
    /// that means leaving the parent — except at depth 0, where nothing is
    /// shallower and the walk therefore spans clusters, keeping the
    /// multi-project list one axis.
    ///
    /// The fallback is not a nicety. A ticket that is an **only child** has no
    /// sibling in either direction, and long single-file chains are the normal
    /// shape of a real map — so a strict sibling walk left the cursor wedged on
    /// them, unable to move at all. Falling through to the adjacent stop means
    /// `↑`/`↓` can always walk the tree and no stop is ever unreachable by them
    /// alone, while a genuine sibling still wins whenever there is one.
    fn move_sibling(&mut self, delta: isize) {
        let stops = self.stops();
        if stops.is_empty() {
            // Nothing on screen, so there is no stop to have chosen — pressing a
            // key over an empty list is not a selection.
            self.cursor = Cursor::Untouched;
            return;
        }
        let pos = self.cursor_pos();
        let depth = stops[pos].depth;
        let mut adjacent = None;
        let mut i = pos as isize;
        loop {
            i += delta;
            let Some(at) = usize::try_from(i).ok().and_then(|i| stops.get(i)) else {
                break; // ran off the end
            };
            let i = i as usize;
            adjacent.get_or_insert(i);
            if at.depth == depth {
                self.cursor = Cursor::Chosen(i);
                return;
            }
            if at.depth < depth {
                break; // left the parent
            }
        }
        // No sibling that way: step one stop, so the cursor is never wedged.
        // `None` only when there is nothing at all in that direction, which is
        // the one case where holding still is the honest answer.
        if let Some(next) = adjacent {
            self.cursor = Cursor::Chosen(next);
        }
    }

    /// `→`: reveal — open a shut group, else move *forward* one stop.
    ///
    /// Stepping forward one stop is what descending *is*: a plan always emits a
    /// node's children immediately after it, so the stop after a ticket with a
    /// subtree is its first child. On a leaf there is nothing to descend into
    /// and the same step carries on to whatever comes next, which is what keeps
    /// the key live everywhere — held down, `→` visits every stop in order.
    fn descend(&mut self) {
        // On the project list there is only one thing deeper than a row, and
        // it is the project: `→` enters it, which is the same move `enter`
        // makes there. Stepping to the next project instead would make the
        // depth key a second `↓` on the one screen where depth means
        // something.
        if let (Level::Projects, Some(Stop::Project(repo))) = (&self.level, self.cursor_stop()) {
            self.enter(&repo);
            return;
        }
        if let Some(Stop::Group(id)) = self.cursor_stop() {
            if !self.expanded.contains(&id) {
                self.expanded.insert(id);
                return; // the rows appear beneath; the cursor stays on the line
            }
        }
        let pos = self.cursor_pos();
        if pos + 1 < self.stops().len() {
            self.cursor = Cursor::Chosen(pos + 1);
        }
    }

    /// `←`: close — shut an open group, else out to the parent, else back one
    /// stop, else out of the project entirely. The mirror of [`App::descend`]:
    /// it only ever moves earlier in the body, and the last clause is what
    /// stops it dying at depth 0.
    ///
    /// That last clause is the **back key**. `←` held down walks out of a
    /// subtree, up its cluster, onto the project row, and then out to the
    /// project list — one key from anywhere on screen to the top level, which
    /// is what `ctrl-g` used to be for. `esc` is deliberately not this: it
    /// still clears the query and quits, so leaving `wf` never needs you to
    /// know how deep you are.
    fn ascend(&mut self) {
        // On the project's own row, "out" is the list it was entered from.
        // Keyed on the stop rather than on position 0, so a sifted screen —
        // whose first stop is a match, not the project — still answers `←`
        // with an ordinary step back.
        if let (Level::Project { .. }, Some(Stop::Project(_))) = (&self.level, self.cursor_stop()) {
            self.leave();
            return;
        }
        if let Some(Stop::Group(id)) = self.cursor_stop() {
            if self.expanded.remove(&id) {
                return; // shut it; the cursor stays on the line
            }
        }
        let stops = self.stops();
        let pos = self.cursor_pos();
        let Some(depth) = stops.get(pos).map(|at| at.depth) else {
            return;
        };
        if depth > 0 {
            if let Some(parent) = (0..pos).rev().find(|&i| stops[i].depth == depth - 1) {
                self.cursor = Cursor::Chosen(parent);
                return;
            }
        }
        if pos > 0 {
            self.cursor = Cursor::Chosen(pos - 1);
        }
    }

    /// Swap in freshly fetched clusters, keeping query/scope/expansions intact.
    ///
    /// What happens to the cursor depends on how it got where it is (#88). A
    /// **chosen** stop is pinned by identity, falling back to the same position,
    /// clamped, if it vanished — see `refresh::preserve_cursor`. An **untouched**
    /// cursor has nothing to pin: it means the top of the list, so it re-derives
    /// to the first stop of whatever just arrived. Maps stream in one fetch at a
    /// time and the order leads on activity, so anchoring the start position too
    /// would let the first map to land drag the cursor downwards the moment a
    /// busier map sorted above it.
    pub fn replace_clusters(&mut self, clusters: BTreeMap<MapId, Map>) {
        let pinned = match self.cursor {
            Cursor::Untouched => None,
            Cursor::Chosen(_) => Some(match self.cursor_key() {
                Some(key) => Pinned::Identity(key, self.cursor_pos()),
                None => Pinned::Position(self.cursor_pos()),
            }),
        };
        self.clusters = clusters;
        let Some(pinned) = pinned else {
            return;
        };
        let new_order = self.stop_keys();
        self.cursor = Cursor::Chosen(pinned.resolve(&new_order));
    }

    /// The first enter (#62): stage a launch of whatever the cursor is on by
    /// opening the launch picker — a ticket, or since #96 the cluster header,
    /// which stages the **map** as one node.
    ///
    /// Two things still refuse, with a count-line notice, and both are
    /// ticket-only. Blocked is refused on *status* (its stage is unactionable,
    /// whatever it is); done is refused by [`launch::Launchable::parse`]
    /// finding no launchable stage — stage, not ticket state, so a merged PR
    /// on a still-open ticket refuses too. Neither can arise on a map: a map
    /// has no blockers and no stage, and a finished one is not drawn.
    ///
    /// Staging **chooses the stop it acts on** (#148), and every arm that
    /// opens the picker goes through [`App::open_launch_picker`] to do it — the
    /// refusals do not, because nothing was staged, so nothing was chosen.
    fn request_launch(&mut self) -> Outcome {
        match self.cursor_stop() {
            // On a group line there is no agent to run, and exactly one thing
            // the key could plausibly mean — so `enter` opens or shuts it
            // rather than reporting that nothing is selected when something
            // plainly is.
            Some(Stop::Group(id)) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
                Outcome::Continue
            }
            Some(Stop::Map(id)) => {
                let staged = Staged::map(&MapRef::new(&id, &self.clusters[&id].title));
                // A map is a node (#96), so a charting session is as
                // resumable as a build one — and rather more worth resuming.
                let staged = self.resumable(staged, id.number);
                self.prewarm(&staged);
                self.open_launch_picker(staged);
                Outcome::Continue
            }
            // One stop, two screens, and the screen decides — which is the
            // only place in the key handler that is true, and is what makes
            // the level a level rather than a filter.
            //
            // On the list, a project is somewhere to *go*: `enter` selects it,
            // exactly as `→` does. On its own screen it is somewhere to
            // *start*: there is no node here, so the picker opens straight
            // onto the creation rows, and since this is also where an
            // untouched cursor sits, `enter` type `enter` files a task in the
            // repo you are standing in without touching a single other key.
            Some(Stop::Project(repo)) => {
                if matches!(self.level, Level::Projects) {
                    self.enter(&repo);
                    return Outcome::Continue;
                }
                self.open_launch_picker(Staged::project(&repo));
                Outcome::Continue
            }
            Some(Stop::Ticket(row)) => self.request_ticket_launch(&row),
            None => {
                self.notice = Some("nothing selected".to_string());
                Outcome::Continue
            }
        }
    }

    /// The ticket half of [`App::request_launch`], split out so the two
    /// refusals sit together and the stop match above stays readable.
    fn request_ticket_launch(&mut self, row: &Row) -> Outcome {
        let ticket = self.ticket(row);
        if let Status::Blocked { needs } = &ticket.status {
            let needs: Vec<String> = needs.iter().map(|n| format!("#{n}")).collect();
            self.notice = Some(format!(
                "#{} is blocked — needs {}",
                ticket.number,
                needs.join(", ")
            ));
            return Outcome::Continue;
        }
        // The map the row was picked in, carried whole: a ticket can sit on a
        // map in another repo, and a bare number would name the wrong issue
        // there (#124).
        let map = MapRef::new(&row.map, &self.clusters[&row.map].title);
        match Staged::ticket(ticket, &map, stage(&ticket.prs, &ticket.status)) {
            None => {
                self.notice = Some(format!("#{} is done — nothing to launch", ticket.number));
                Outcome::Continue
            }
            Some(staged) => {
                let staged = self.resumable(staged, ticket.number);
                self.prewarm(&staged);
                self.open_launch_picker(staged);
                Outcome::Continue
            }
        }
    }

    /// Put the launch picker up on `staged` — **the only place it opens**, as
    /// distinct from the key handler that re-seats one already up, so the
    /// cursor write below cannot be forgotten by an arm added later.
    ///
    /// Staging chooses the stop it acts on (#148): the cursor is recorded as
    /// [`Cursor::Chosen`] *before* the overlay is set. To the human the launch
    /// *is* a selection of that row, however the cursor arrived on it — an
    /// untouched cursor sitting on the first match is enough — and without the
    /// write a refresh would re-derive "the top" and carry the cursor off the
    /// very row whose launch is being picked. The write and the overlay are
    /// one statement pair in one function precisely so that they cannot come
    /// apart: a new stop worth staging reaches the picker through here, and
    /// arrives with its row already chosen.
    fn open_launch_picker(&mut self, staged: Staged) {
        self.cursor = Cursor::Chosen(self.cursor_pos());
        self.overlay = Overlay::PickLaunch {
            candidate: staged.default_candidate(),
            staged,
            agent: Agent::default(),
            steer: String::new(),
        };
    }

    /// Start warming the staged node's container while the launch picker is
    /// up: the human's mode-and-steer pause is exactly the window a cold
    /// `devpod up` needs a head start on.
    ///
    /// Nothing happens unless [`launch::prewarm_enabled`] says so — staging
    /// must stay a keystroke that creates nothing for anyone who has not asked
    /// for this. Beyond that: host launches plan nothing, a node already
    /// warmed this session is not warmed twice, and the spawn is
    /// fire-and-forget — the launch itself neither depends on it nor waits for
    /// it, beyond `dl`'s own per-workspace serialization.
    fn prewarm(&mut self, staged: &Staged) {
        self.warm(staged, launch::prewarm_enabled());
    }

    /// The whole of it, with the gate passed in rather than read from the
    /// environment, so the rules below are testable without a container, a
    /// `dl` on `PATH`, or a mutated process environment. Answers what was
    /// recorded, and `None` when this staging is not the one that warms
    /// `staged` at all.
    ///
    /// **The instant is taken from the spawn, not from the claim**, because
    /// only a prewarm that actually went out has one: a node whose launch is
    /// host-only is claimed all the same — it plans nothing now and would plan
    /// nothing later, and remembering that saves re-walking the checkouts on
    /// every re-stage — but recording an instant for it would name a
    /// `dl <ws> up` that never happened, and the launch would hand `dl` a
    /// prewarm to weigh itself against that nobody fired (#160).
    fn warm(&mut self, staged: &Staged, enabled: bool) -> Option<Prewarmed> {
        let workspace = self.workspace_to_warm(staged, enabled)?;
        let recorded = match launch::prewarm(&self.checkouts, staged) {
            Some(argv) => {
                launch::spawn_detached(&argv);
                Prewarmed::At(SystemTime::now())
            }
            None => Prewarmed::Nothing,
        };
        self.prewarmed.insert(workspace, recorded);
        Some(recorded)
    }

    /// The workspace this staging gets to warm, or `None` if it does not get
    /// to. Split from the spawn deliberately: the gate and the once-per-node
    /// rule are the parts with logic in them.
    ///
    /// A **question, not a claim** — `&self`, and asking twice answers twice.
    /// The once-per-node rule is enforced by the insert in [`warm`](Self::warm),
    /// which is the only writer of `prewarmed`; this names the key that insert
    /// will land under, and answers `None` where there will be no insert.
    ///
    /// Nothing is offered while the prewarm is off — `?` short-circuits — so a
    /// session that exports `WF_PREWARM` and re-stages a node still warms it.
    /// A stop with no workspace to warm (a project row) is offered nothing
    /// either, because there is nothing to record it under.
    fn workspace_to_warm(&self, staged: &Staged, enabled: bool) -> Option<String> {
        let workspace = staged.node_workspace()?;
        (enabled && !self.prewarmed.contains_key(&workspace)).then_some(workspace)
    }

    /// When this session fired a prewarm for what `launch` is about to attach
    /// to, if it fired one — the second half of what a launch hands across the
    /// exec seam ([`launch::Handoff`], #160).
    ///
    /// Keyed on the launch's own workspace rather than on whatever was last
    /// staged: the two are the same node in the ordinary case, and where they
    /// are not — a creation, whose workspace is the repo's default — the
    /// answer must be nothing rather than another node's instant.
    pub fn prewarm_fired(&self, launch: &Launch) -> Option<SystemTime> {
        match self.prewarmed.get(&launch.workspace()) {
            Some(Prewarmed::At(fired)) => Some(*fired),
            Some(Prewarmed::Nothing) | None => None,
        }
    }

    /// The second enter: resolve the staged launch against the projects cache
    /// — straight to the loop when there is one candidate checkout, through
    /// the picker when there are several, and a notice when there is none to
    /// launch into. Which map the ticket belongs to is the cluster it sits in
    /// — a row without a map is unrepresentable, so the old "repo has no map"
    /// failure is gone with it.
    ///
    /// Everything this needs came with the [`Staged`] launch and the picked
    /// [`Candidate`] — including `route`, carried from the row that was drawn
    /// rather than derived a second time — so a refetch between the two enters
    /// cannot redirect it at another ticket or another skill.
    fn resolve_launch(&mut self, staged: &Staged, route: Route, mode: &LaunchMode) -> Outcome {
        let targets = launch::plan(&self.checkouts, staged, route, mode);
        self.act_on(targets, &staged.repo, &staged.key())
    }

    /// The creation half: the same resolution against the same cache, for a
    /// launch that has no node to name (#114). The creation arrives already
    /// complete — [`launch::CreationKind::with_text`] refused the empty task
    /// before this was called — so the only thing left to answer is *where*.
    fn resolve_creation(
        &mut self,
        repo: &str,
        creation: &launch::Creation,
        agent: Agent,
    ) -> Outcome {
        let targets = launch::plan_create(&self.checkouts, repo, creation, agent);
        self.act_on(targets, repo, "+new")
    }

    /// What a resolved [`Targets`] does to the screen: launch, prompt for the
    /// tree, or say there is none. Shared by both resolutions so a creation
    /// cannot drift into reporting its checkouts differently from a node.
    fn act_on(&mut self, targets: Targets, repo: &str, key: &str) -> Outcome {
        match targets {
            Targets::Unregistered => {
                self.notice = Some(format!(
                    "no registered checkout of {repo} on this machine — run wf inside one"
                ));
                Outcome::Continue
            }
            Targets::One(launch) => {
                self.notice = Some(format!("→ {}", launch.describe()));
                Outcome::Launch(Box::new(launch))
            }
            Targets::Many(launches) => {
                self.notice = Some(format!("{repo}{key}: which checkout?"));
                self.overlay = Overlay::PickCheckout {
                    launches,
                    cursor: 0,
                };
                Outcome::Continue
            }
        }
    }

    /// Keys while the launch picker is up (#62, restaged as an overlay). It
    /// owns every printable key — filtering already happened, and this text
    /// steers the launch — and esc backs out to the list with the query and
    /// cursor untouched (they were never touched to begin with; the invariant
    /// is that nothing here may touch them).
    ///
    /// The arrows move the mode and the letters type: no printable key picks a
    /// mode, so `auto` typed into the steer field steers, and `j`/`k` — which
    /// walk the which-checkout picker, a modal with nothing to type into — are
    /// text here.
    fn handle_pick_launch_key(
        &mut self,
        key: KeyEvent,
        staged: Staged,
        mut candidate: Candidate,
        mut agent: Agent,
        mut steer: String,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Back to the list; the overlay is already None.
            KeyCode::Esc => return Outcome::Continue,
            KeyCode::Char('c') if ctrl => return Outcome::Quit,
            // Returns *before* the overlay is put back, because resolving may
            // have opened the which-checkout modal over this one — falling
            // through would overwrite it with the picker the launch just left.
            // The one exception is the refusal below, which puts this very
            // picker back so the missing task can be typed into it.
            KeyCode::Enter => match candidate {
                Candidate::Launch { mode, route } => {
                    return self.resolve_launch(
                        &staged,
                        route,
                        &LaunchMode::picked(agent, mode, &steer),
                    )
                }
                // A resume needs no resolution: the record already names the
                // one tree its conversation lives in, so there is nothing for
                // the checkout picker to ask about and no `Targets` to walk.
                Candidate::Resume { .. } => {
                    if let Some(launch) = launch::resume_launch(&staged, &steer) {
                        self.notice = Some(format!("→ {}", launch.describe()));
                        return Outcome::Launch(Box::new(launch));
                    }
                    // Unreachable: the row is built from the record, so a
                    // resume row without one cannot be drawn to be picked.
                    self.notice = Some("nothing to resume here".to_string());
                }
                Candidate::Create(kind) => match kind.with_text(&steer) {
                    Some(creation) => return self.resolve_creation(&staged.repo, &creation, agent),
                    // The one per-row refusal (#114): `/wf-one` with no task is
                    // meaningless, so it is refused where a done or blocked node
                    // is — on the count line, with the picker still up.
                    None => {
                        self.notice = Some(format!("type the {} first", kind.field()));
                    }
                },
            },
            // The two directions are genuinely different steps now that there
            // is a third mode (#112) and the creation rows (#114); `tab` joins
            // the arrows because it is what the rest of the screen uses to move
            // through a list, and it steps the way `down` does rather than
            // toggling.
            // Dead on the resume row, and only there: that row's agent comes
            // from the record — a Claude conversation is not rejoinable by
            // Codex — so moving the axis could only put a title over the row
            // that disagreed with what `enter` runs.
            KeyCode::Left | KeyCode::Right if !matches!(candidate, Candidate::Resume { .. }) => {
                agent = next_agent(agent);
            }
            KeyCode::Up => candidate = previous_candidate(&staged, candidate),
            KeyCode::Down | KeyCode::Tab => candidate = next_candidate(&staged, candidate),
            KeyCode::Backspace => {
                steer.pop();
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                steer.push(c);
            }
            _ => {}
        }
        self.overlay = Overlay::PickLaunch {
            staged,
            candidate,
            agent,
            steer,
        };
        Outcome::Continue
    }

    /// Keys while the checkout picker is up. The modal owns every key: no
    /// typing leaks into the query behind it.
    fn handle_overlay_key(
        &mut self,
        key: KeyEvent,
        launches: Vec<Launch>,
        cursor: usize,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = launches.len().saturating_sub(1);
        let moved = |cursor: usize, delta: isize| {
            (cursor as isize + delta).clamp(0, last as isize) as usize
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = Overlay::None;
                self.notice = Some("launch cancelled".to_string());
                Outcome::Continue
            }
            KeyCode::Char('c') if ctrl => Outcome::Quit,
            KeyCode::Enter => {
                self.overlay = Overlay::None;
                let launch = launches
                    .into_iter()
                    .nth(cursor)
                    .expect("picker cursor stays in range");
                self.notice = Some(format!("→ {}", launch.describe()));
                Outcome::Launch(Box::new(launch))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Overlay::PickCheckout {
                    launches,
                    cursor: moved(cursor, 1),
                };
                Outcome::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Overlay::PickCheckout {
                    launches,
                    cursor: moved(cursor, -1),
                };
                Outcome::Continue
            }
            _ => {
                self.overlay = Overlay::PickCheckout { launches, cursor };
                Outcome::Continue
            }
        }
    }

    /// Handle one keypress. Typing edits the query (rows re-filter, cursor
    /// jumps to the first visible row); see the ticket #14 skeleton for the
    /// chord bindings.
    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        self.notice = None;
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::PickCheckout { launches, cursor } => {
                return self.handle_overlay_key(key, launches, cursor);
            }
            Overlay::PickLaunch {
                staged,
                candidate,
                agent,
                steer,
            } => {
                return self.handle_pick_launch_key(key, staged, candidate, agent, steer);
            }
            Overlay::None => {}
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Outcome::Quit,
            // `ctrl-f`, `ctrl-g` and the refresh chord are gone, not merely
            // undocumented, and they fall through to nothing here rather than
            // being caught and ignored — an unbound key is unbound.
            //
            // The first two named a move the arrows already make: focusing a
            // project is entering it and widening is `←` out of it. Refresh
            // named something nothing else does — refetch every map in place,
            // keeping the query, the level and the cursor — so retiring it is a
            // trade rather than a tidy-up. What it bought was the one screen
            // update a session could have; what it cost was a *second write* to
            // every piece of state behind that screen, and each of those writes
            // needed machinery of its own: a `Loaders::restart` so a refetch
            // could not be beaten by the load it replaced, a way to put
            // [`crate::refresh::Startup`] back into loading, and a generation
            // tag on the background reading so an answer to a withdrawn
            // question could be told from a live one. All of it went with the
            // key, and everything the loop folds is now written once.
            //
            // The price is paid by anyone who closes a ticket in the browser
            // and wants to see it: run `wf` again — ~0.6 s warm — and lose the
            // query, the project you had entered and where the cursor was.

            // Toggle the structural lens (#51): leverage ⇄ forest. The cursor
            // stays on its ticket if the other screen shows it; a live query
            // keeps flattening either lens until it is cleared.
            KeyCode::Tab => {
                let anchor = self.cursor_anchor();
                self.lens = self.lens.toggled();
                if let Some(key) = anchor {
                    self.point_at(&key);
                }
                Outcome::Continue
            }
            // `↑`/`↓` walk siblings at the cursor's depth (#57) — the pick
            // axis, stepping over the context beneath a ticket rather than
            // through it.
            KeyCode::Down => {
                self.move_sibling(1);
                Outcome::Continue
            }
            KeyCode::Up => {
                self.move_sibling(-1);
                Outcome::Continue
            }
            KeyCode::Char('j') if ctrl => {
                self.move_sibling(1);
                Outcome::Continue
            }
            KeyCode::Char('k') if ctrl => {
                self.move_sibling(-1);
                Outcome::Continue
            }
            // `→`/`←` are the depth axis: reveal what the cursor is on, or
            // close it again.
            KeyCode::Right => {
                self.descend();
                Outcome::Continue
            }
            KeyCode::Left => {
                self.ascend();
                Outcome::Continue
            }
            // The whole point of the picker: enter runs the agent here, and
            // there is nothing to come back to.
            KeyCode::Enter => self.request_launch(),
            KeyCode::Esc => {
                if self.query.is_empty() {
                    Outcome::Quit
                } else {
                    self.query.clear();
                    self.cursor = Cursor::Untouched;
                    Outcome::Continue
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = Cursor::Untouched;
                Outcome::Continue
            }
            // `q` quits only on an empty query; mid-query it types.
            KeyCode::Char('q') if !ctrl && self.query.is_empty() => Outcome::Quit,
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.cursor = Cursor::Untouched;
                Outcome::Continue
            }
            _ => Outcome::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{CreationKind, Isolation, Mode, Route};
    use crate::model::{classify, Checks, PrLink, PrStatus, Review, TicketType};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

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

    /// The one project nearly every fixture below stands on. Named because a
    /// project screen is a repo's, so the slug is now load-bearing in a way it
    /// was not when every fixture's clusters simply all rendered.
    const PROJECT: &str = "blooop/wayfinder";

    /// An app standing on `repo`'s screen — the level every cluster test is
    /// about. `App::new` opens on the project *list*, which is what `wf` run
    /// outside a checkout shows and which draws no cluster at all, so a test
    /// about clusters has to say which project's it means.
    fn app_on(repo: &str, clusters: BTreeMap<MapId, Map>) -> App {
        let mut app = App::new(clusters);
        app.enter(repo);
        app
    }

    /// Two clusters on one project's screen: the wayfinder map and a dotfiles
    /// map. Both repos stay registered — a checkout of each — because the
    /// which-checkout and cross-repo tests below step between their screens.
    fn fixture_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                last_activity: None,
                tickets: vec![
                    ticket(
                        "blooop/wayfinder",
                        2,
                        "Choose the stack",
                        false,
                        true,
                        vec![],
                    ),
                    ticket(
                        "blooop/wayfinder",
                        6,
                        "Re-entry breadcrumbs",
                        true,
                        false,
                        vec![],
                    ),
                    ticket(
                        "blooop/wayfinder",
                        7,
                        "Supervising AFK agents",
                        true,
                        false,
                        vec![6],
                    ),
                    ticket(
                        "blooop/wayfinder",
                        9,
                        "Main screen design",
                        true,
                        true,
                        vec![],
                    ),
                ],
            },
        );
        clusters.insert(
            MapId::new("blooop/dotfiles", 4),
            Map {
                title: "Map: dotfiles".to_string(),
                last_activity: None,
                tickets: vec![ticket(
                    "blooop/dotfiles",
                    103,
                    "Prune legacy bash aliases",
                    true,
                    false,
                    vec![],
                )],
            },
        );
        app_on("blooop/wayfinder", clusters)
    }

    /// A one-ticket cluster with a given last-activity stamp, open or finished.
    fn cluster(repo: &str, number: u64, stamp: Option<&str>, open: bool) -> (MapId, Map) {
        (
            MapId::new(repo, number),
            Map {
                title: format!("Map: {repo}"),
                last_activity: stamp.map(|s| Activity::parse(s).expect("fixture stamp parses")),
                tickets: vec![ticket(repo, 1, "only ticket", open, false, vec![])],
            },
        )
    }

    fn cluster_order(app: &App) -> Vec<MapId> {
        app.scoped_clusters()
            .into_iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[test]
    fn clusters_order_by_activity_with_finished_maps_last() {
        // The bug this fixes: `blooop/finished` is both the lowest id *and* the
        // most recently touched, and it still belongs at the bottom — a map with
        // nothing left to do is history, however fresh.
        let clusters: BTreeMap<MapId, Map> = [
            cluster(PROJECT, 1, Some("2026-08-06T12:00:00Z"), false),
            cluster(PROJECT, 2, Some("2026-08-01T09:00:00Z"), true),
            cluster(PROJECT, 3, Some("2026-08-05T09:00:00Z"), true),
            cluster(PROJECT, 4, None, true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            cluster_order(&app_on(PROJECT, clusters)),
            vec![
                MapId::new(PROJECT, 3),
                MapId::new(PROJECT, 2),
                // An unparsed stamp is not guessed into the middle of the live
                // maps: it sorts after every dated one…
                MapId::new(PROJECT, 4),
                // …and the finished map is still below it.
                MapId::new(PROJECT, 1),
            ]
        );
    }

    #[test]
    fn equal_activity_falls_back_to_the_map_id() {
        // Same instant on three maps: the order has to be *some* fixed one, or
        // the screen reshuffles itself between frames for no reason. The
        // tie-break is the whole `MapId`; on one project's screen — the only
        // kind there is — that is the map number.
        let stamp = Some("2026-08-06T12:00:00Z");
        let clusters: BTreeMap<MapId, Map> = [
            cluster(PROJECT, 47, stamp, true),
            cluster(PROJECT, 4, stamp, true),
            cluster(PROJECT, 1, stamp, true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            cluster_order(&app_on(PROJECT, clusters)),
            vec![
                MapId::new(PROJECT, 1),
                MapId::new(PROJECT, 4),
                MapId::new(PROJECT, 47),
            ]
        );
    }

    #[test]
    fn an_untouched_cursor_sits_on_the_project_row_whatever_streams_in() {
        // What #88 and #89 were fighting for, won outright by the project row:
        // an untouched cursor used to mean "the top row", and the top row was
        // whichever map had arrived and sorted highest — so the default
        // selection moved under the human as fetches landed. The first stop is
        // now the project itself, which is known from a local `git` call before
        // any fetch and cannot be outranked by anything that arrives later.
        let mut app = app_on(
            PROJECT,
            BTreeMap::from([cluster(PROJECT, 1, Some("2026-08-01T00:00:00Z"), true)]),
        );
        assert_eq!(app.cursor_pos(), 0);
        assert_eq!(app.cursor_stop(), Some(Stop::Project(PROJECT.to_string())));

        let mut both = BTreeMap::new();
        both.extend([cluster(PROJECT, 1, Some("2026-08-01T00:00:00Z"), true)]);
        both.extend([cluster(PROJECT, 2, Some("2026-08-07T00:00:00Z"), true)]);
        app.replace_clusters(both);

        assert_eq!(app.cursor_pos(), 0, "a fresher map cannot move it");
        assert_eq!(app.cursor_stop(), Some(Stop::Project(PROJECT.to_string())));
    }

    /// Two live maps of one project, the second of them older, so a map fresher
    /// than both sorts above the pair and pushes every existing stop down by
    /// two — its header and its row (#96).
    fn streaming_pair() -> BTreeMap<MapId, Map> {
        [
            cluster(PROJECT, 1, Some("2026-08-01T00:00:00Z"), true),
            cluster(PROJECT, 3, Some("2026-07-01T00:00:00Z"), true),
        ]
        .into_iter()
        .collect()
    }

    /// The pair with a fresher map added — it renders first, so every original
    /// stop moves down two.
    fn streaming_trio() -> BTreeMap<MapId, Map> {
        let mut all = streaming_pair();
        all.extend([cluster(PROJECT, 2, Some("2026-08-07T00:00:00Z"), true)]);
        all
    }

    #[test]
    fn a_chosen_row_stays_with_its_ticket_when_a_map_sorts_above_it() {
        // The #50/#57 behaviour, which #88 must not cost: a row someone moved to
        // is pinned by identity, so an arriving map slides it down the screen
        // rather than stealing the selection.
        let mut app = app_on(PROJECT, streaming_pair());
        // Past the project row, then map #1's header and row, onto #3's.
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.cursor_pos(), 4);
        assert_eq!(
            app.cursor_row().map(|row| row.map),
            Some(MapId::new(PROJECT, 3))
        );

        app.replace_clusters(streaming_trio());

        assert_eq!(app.cursor_pos(), 6, "the fresher map pushed the row down");
        assert_eq!(
            app.cursor_row().map(|row| row.map),
            Some(MapId::new(PROJECT, 3)),
            "still the row they chose"
        );
    }

    #[test]
    fn choosing_the_top_row_pins_it_rather_than_re_defaulting_to_the_top() {
        // The case a sentinel cannot serve. This cursor sits on the first
        // *ticket*, and must behave like the opposite of a fresh one: the human
        // put it there, so a map arriving above carries it down rather than
        // leaving it on whatever is now first.
        let mut app = app_on(PROJECT, streaming_pair());
        for _ in 0..2 {
            app.handle_key(key(KeyCode::Down)); // project row, header, row
        }
        assert_eq!(app.cursor_pos(), 2);
        assert_eq!(
            app.cursor_row().map(|row| row.map),
            Some(MapId::new(PROJECT, 1))
        );

        app.replace_clusters(streaming_trio());

        assert_eq!(
            app.cursor_row().map(|row| row.map),
            Some(MapId::new(PROJECT, 1)),
            "their stop, not whatever is on top now"
        );
        assert_eq!(app.cursor_pos(), 4);
    }

    #[test]
    fn toggling_the_lens_does_not_turn_an_untouched_cursor_into_a_choice() {
        // `tab` rebuilds the list under the cursor and keeps it on its stop —
        // but keeping an *untouched* cursor on the row that merely happens to
        // be first would anchor a default, letting #88 back in through a key
        // that was never about the selection at all.
        let mut app = app_on(PROJECT, streaming_pair());
        app.handle_key(key(KeyCode::Tab));

        app.replace_clusters(streaming_trio());

        assert_eq!(app.cursor_pos(), 0);
        assert_eq!(app.cursor_stop(), Some(Stop::Project(PROJECT.to_string())));
    }

    /// One map with finished work in it, so the leverage lens gives it a `Done`
    /// group line — a stop the forest lens, which has no group lines at all, does
    /// not have.
    fn map_with_a_done_group() -> BTreeMap<MapId, Map> {
        BTreeMap::from([(
            MapId::new(PROJECT, 1),
            Map {
                title: "Map: the slow one".to_string(),
                last_activity: Some(
                    Activity::parse("2026-08-01T00:00:00Z").expect("fixture stamp parses"),
                ),
                tickets: vec![
                    ticket(PROJECT, 1, "takeable", true, false, vec![]),
                    ticket(PROJECT, 2, "finished", false, false, vec![]),
                ],
            },
        )])
    }

    #[test]
    fn a_lens_toggle_off_a_vanished_group_line_leaves_no_choice_behind() {
        // #88, re-entered through the lens door. The forest lens emits no group
        // lines, so `tab` from a `Done` line deletes the very stop being held,
        // and recording a position for the stop that vanished is a default
        // written down as a choice — the one state `Cursor` exists to forbid.
        //
        // The old symptom (the next map to sort above drags the cursor off the
        // top row) can no longer be *observed* on a project screen: the first
        // stop is the project row now, so an untouched cursor and a
        // wrongly-recorded `Chosen(0)` resolve to the same line. What is still
        // worth pinning is that the toggle lands the cursor there rather than
        // on some stop the human never picked.
        let mut app = app_on(PROJECT, map_with_a_done_group());
        while !matches!(app.cursor_stop(), Some(Stop::Group(_))) {
            app.handle_key(key(KeyCode::Down));
        }

        app.handle_key(key(KeyCode::Tab)); // the forest has no group line to keep

        let mut fresher = map_with_a_done_group();
        fresher.extend([cluster(PROJECT, 2, Some("2026-08-07T00:00:00Z"), true)]);
        app.replace_clusters(fresher);

        assert_eq!(app.cursor_pos(), 0);
        assert_eq!(
            app.cursor_stop(),
            Some(Stop::Project(PROJECT.to_string())),
            "nothing was chosen, so the cursor still means the top of the list"
        );
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_key_over_an_empty_list_is_not_a_selection() {
        // The sift has emptied the screen, so `↓` has nothing to land on. If
        // that keypress were written down as a choice, the rows arriving next
        // would find the cursor pinned to position 0 — the map header a sifted
        // screen leads with — and `enter` there would stage the whole map
        // rather than the row the query was typed for.
        let mut app = fixture_app();
        type_str(&mut app, "zzz");
        assert!(app.stops().is_empty(), "the query should match nothing");

        app.handle_key(key(KeyCode::Down));

        let mut fresher = app.clusters.clone();
        fresher.insert(
            MapId::new(PROJECT, 99),
            Map {
                title: "Map: late arrival".to_string(),
                last_activity: None,
                tickets: vec![ticket(PROJECT, 200, "zzz sleeper", true, false, vec![])],
            },
        );
        app.replace_clusters(fresher);

        assert_eq!(
            app.cursor_ticket().map(|t| t.number),
            Some(200),
            "nothing was chosen over the empty list, so the cursor means the top row"
        );
    }

    #[test]
    fn staging_a_launch_chooses_the_row_it_acts_on() {
        // `enter` on a row the cursor merely defaulted to is still an act *on
        // that row*: the human launched from it. If staging left the cursor
        // untouched, the next refresh would re-derive "the top", and a fresher
        // map sorting above would carry the cursor off the very row whose
        // launch was just staged — the launch and the choice drifting apart.
        let mut app = fixture_app();
        type_str(&mut app, "bread"); // one match: #6, reached by default, not by `↓`
        assert_eq!(app.cursor_ticket().map(|t| t.number), Some(6));

        app.handle_key(key(KeyCode::Enter)); // stage a launch of #6
        assert!(matches!(app.overlay, Overlay::PickLaunch { .. }));
        app.handle_key(key(KeyCode::Esc)); // back to the list, nothing picked

        let mut fresher = app.clusters.clone();
        fresher.insert(
            MapId::new(PROJECT, 99),
            Map {
                title: "Map: fresher".to_string(),
                last_activity: Some(
                    Activity::parse("2026-08-07T00:00:00Z").expect("fixture stamp parses"),
                ),
                tickets: vec![ticket(PROJECT, 200, "breadwinner", true, false, vec![])],
            },
        );
        app.replace_clusters(fresher);

        assert_eq!(
            app.cursor_ticket().map(|t| t.number),
            Some(6),
            "the launch was staged from #6, so the refresh keeps the cursor on it"
        );
    }

    #[test]
    fn typing_filters_and_cursor_lands_on_first_visible_row() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Down)); // move off row 0 first
        type_str(&mut app, "bread");
        assert_eq!(app.query, "bread");
        let visible = app.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        assert_eq!(app.cursor_pos(), 1);
    }

    /// What the cursor is on, as a short label: a ticket by number, a map by
    /// `map #n`, a group by kind. Reads like the screen does.
    fn at(app: &App) -> String {
        match app.cursor_stop() {
            Some(Stop::Map(id)) => format!("map #{}", id.number),
            Some(Stop::Ticket(row)) => format!("#{}", app.ticket(&row).number),
            Some(Stop::Group(g)) => format!("{:?}", g.kind),
            Some(Stop::Project(repo)) => format!("project {repo}"),
            None => "nothing".to_string(),
        }
    }

    /// Walk the cursor down to the stop [`at`] labels `label`.
    ///
    /// Tests that only need to *be somewhere* say where with this rather than
    /// counting `Down` presses: a count is a claim about the whole stop list,
    /// so every change to what counts as a stop — cluster headers becoming one
    /// (#96), the group lines of #57 before them — silently invalidates every
    /// such count at once. The tests that are genuinely *about* navigation
    /// still press the keys.
    fn go_to(app: &mut App, label: &str) {
        for _ in 0..=app.stops().len() {
            if at(app) == label {
                return;
            }
            app.handle_key(key(KeyCode::Down));
        }
        panic!("no stop labelled {label} on this screen");
    }

    #[test]
    fn down_walks_the_takeable_tickets_and_steps_over_their_context() {
        // The depth-0 axis of a project's screen: its own row first (#114's
        // creation stop, now the place the cursor starts), then each cluster's
        // header (#96) and that cluster's takeable rows — #6 and #9 — then its
        // Done group. #7 hangs under #6 as context and is *not* on this axis,
        // which is the whole point of #57.
        let mut app = fixture_app();
        assert_eq!(
            at(&app),
            "project blooop/wayfinder",
            "the default is the project, not a ticket"
        );
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "map #1");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#6");
        app.handle_key(ctrl('j'));
        assert_eq!(at(&app), "#9", "#7 is context under #6, not a stop here");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "Done");
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(at(&app), "Done", "the last stop holds");
    }

    #[test]
    fn right_descends_into_the_subtree_and_left_comes_back() {
        let mut app = fixture_app();
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(at(&app), "#7", "→ steps into what #6 unblocks");
        // #7 is an only child, so there is no sibling to walk to — and ↓ must
        // still move rather than wedge, so it steps to the adjacent stop.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#9");
        // ↑ from #9 finds *its* own sibling #6 rather than diving back into
        // #6's subtree: a real sibling always beats the fallback.
        app.handle_key(key(KeyCode::Up));
        assert_eq!(at(&app), "#6");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(at(&app), "#7");
        app.handle_key(key(KeyCode::Left));
        assert_eq!(at(&app), "#6", "← returns to the parent");
        // At depth 0 there is no parent to climb to, so ← keeps its promise the
        // only way left: one stop back — which, from a cluster's first row, is
        // that cluster's own header.
        app.handle_key(key(KeyCode::Left));
        assert_eq!(at(&app), "map #1");
    }

    #[test]
    fn down_walks_siblings_when_a_root_unblocks_several() {
        // Three dependents of one root: inside the subtree, ↓ steps between
        // them and stops at the last rather than leaking back out to depth 0.
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                last_activity: None,
                tickets: vec![
                    ticket("blooop/wayfinder", 6, "root", true, false, vec![]),
                    ticket("blooop/wayfinder", 7, "dep a", true, false, vec![6]),
                    ticket("blooop/wayfinder", 8, "dep b", true, false, vec![6]),
                    ticket("blooop/wayfinder", 9, "dep c", true, false, vec![6]),
                ],
            },
        );
        let mut app = app_on(PROJECT, clusters);
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(at(&app), "#7");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#8");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#9");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#9", "held at the last sibling");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(at(&app), "#8");
    }

    /// A cluster with every awkward shape at once: a root whose only child has
    /// children of its own (the chain that wedged), a second root with real
    /// siblings beneath it, a childless root, and done work behind a group.
    fn knotty_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/bencher", 1064),
            Map {
                title: "Map: endgame".to_string(),
                last_activity: None,
                tickets: vec![
                    ticket("blooop/bencher", 1, "done", false, false, vec![]),
                    ticket("blooop/bencher", 10, "root, chained", true, false, vec![]),
                    ticket("blooop/bencher", 11, "only child", true, false, vec![10]),
                    ticket("blooop/bencher", 12, "grandchild a", true, false, vec![11]),
                    ticket("blooop/bencher", 13, "grandchild b", true, false, vec![11]),
                    ticket("blooop/bencher", 20, "root, forked", true, false, vec![]),
                    ticket("blooop/bencher", 21, "child a", true, false, vec![20]),
                    ticket("blooop/bencher", 22, "child b", true, false, vec![20]),
                    ticket("blooop/bencher", 30, "root, barren", true, false, vec![]),
                ],
            },
        );
        app_on("blooop/bencher", clusters)
    }

    #[test]
    fn every_direction_key_always_does_something_unless_it_is_at_that_end() {
        // The rule the human asked for, as a property rather than an example:
        // holding *any* direction key down keeps navigating. A key may only sit
        // still when it is already against its own end of the body — otherwise
        // it must move the cursor or fold something.
        for forward in [true, false] {
            for arrow in [true, false] {
                let code = match (forward, arrow) {
                    (true, true) => KeyCode::Down,
                    (false, true) => KeyCode::Up,
                    (true, false) => KeyCode::Right,
                    (false, false) => KeyCode::Left,
                };
                // Try it from every reachable position, with the group both
                // shut and open, since folding changes the stop list.
                for open_group in [false, true] {
                    // A fresh app per start, because a key is allowed to change
                    // the *screen* and not just the cursor: `←` on the project
                    // row leaves for the project list, and every later start
                    // would then be probing a screen this loop never meant to
                    // be on.
                    let build = || {
                        let mut app = knotty_app();
                        if open_group {
                            while !matches!(app.cursor_stop(), Some(Stop::Group(_))) {
                                app.handle_key(key(KeyCode::Down));
                            }
                            app.handle_key(key(KeyCode::Right));
                            app.cursor = Cursor::Chosen(0);
                        }
                        app
                    };
                    let total = build().stops().len();
                    for start in 0..total {
                        let mut app = build();
                        app.cursor = Cursor::Chosen(start);
                        let before = (app.cursor_pos(), app.stops().len());
                        app.handle_key(key(code));
                        let after = (app.cursor_pos(), app.stops().len());
                        let at_its_end = if forward {
                            start + 1 == total
                        } else {
                            start == 0
                        };
                        if at_its_end {
                            continue; // allowed to hold still, nothing beyond
                        }
                        assert_ne!(
                            before,
                            after,
                            "{code:?} stalled at stop {start} of {total} \
                             (group {}): a direction key must always navigate",
                            if open_group { "open" } else { "shut" }
                        );
                        // …and it must go the way it was asked to go.
                        if after.1 == before.1 {
                            if forward {
                                assert!(after.0 > before.0, "{code:?} went backwards");
                            } else {
                                assert!(after.0 < before.0, "{code:?} went forwards");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn holding_a_direction_key_walks_all_the_way_to_the_end() {
        // The consequence of the property above: each key terminates against
        // its end rather than looping or stalling part-way. `→` is the one that
        // visits *every* stop, since it steps one at a time.
        let mut app = knotty_app();
        // From the very first stop, not the default one — the default skips
        // the cluster header (#96), and `→` only ever moves forward, so
        // starting there would leave one stop unvisited for a reason that has
        // nothing to do with the property being measured.
        app.handle_key(key(KeyCode::Up));
        let mut seen = vec![app.cursor_pos()];
        for _ in 0..40 {
            app.handle_key(key(KeyCode::Right));
            seen.push(app.cursor_pos());
        }
        // `→` opened the group it passed through, so the body it finished
        // walking is larger than the one it started on — measure it now.
        let total = app.stops().len();
        assert_eq!(app.cursor_pos(), total - 1, "→ reached the end");
        let visited: BTreeSet<usize> = seen.into_iter().collect();
        assert_eq!(visited.len(), total, "→ visited every stop");

        // The other three settle against their own end too, from the far side.
        for (code, forward) in [
            (KeyCode::Left, false),
            (KeyCode::Up, false),
            (KeyCode::Down, true),
        ] {
            let mut app = knotty_app();
            let total = app.stops().len();
            app.cursor = Cursor::Chosen(if forward { 0 } else { total - 1 });
            for _ in 0..40 {
                app.handle_key(key(code));
            }
            let end = if forward { app.stops().len() - 1 } else { 0 };
            assert_eq!(app.cursor_pos(), end, "{code:?} settled at its end");
        }
    }

    #[test]
    fn an_only_child_never_wedges_the_cursor() {
        // The live shape that exposed this (bencher#1064): a root whose single
        // dependent has dependents of its own, so the middle ticket has no
        // sibling in either direction. A strict sibling walk left ↑/↓ inert
        // there — every stop must stay reachable by them alone.
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/bencher", 1064),
            Map {
                title: "Map: endgame".to_string(),
                last_activity: None,
                tickets: vec![
                    ticket("blooop/bencher", 1069, "root", true, false, vec![]),
                    ticket(
                        "blooop/bencher",
                        1070,
                        "only child",
                        true,
                        false,
                        vec![1069],
                    ),
                    ticket(
                        "blooop/bencher",
                        1071,
                        "grandchild a",
                        true,
                        false,
                        vec![1070],
                    ),
                    ticket(
                        "blooop/bencher",
                        1072,
                        "grandchild b",
                        true,
                        false,
                        vec![1070],
                    ),
                ],
            },
        );
        let mut app = app_on("blooop/bencher", clusters);
        go_to(&mut app, "#1069");
        app.handle_key(key(KeyCode::Right)); // into #1070
        assert_eq!(at(&app), "#1070");

        // Down has no depth-1 sibling ahead: it steps on rather than freezing.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#1071");
        // …and now there *is* a sibling, so the sibling wins.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#1072");
        // Up back out the same way: sibling first, then the adjacent stop.
        app.handle_key(key(KeyCode::Up));
        assert_eq!(at(&app), "#1071");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(at(&app), "#1070");
        app.handle_key(key(KeyCode::Up));
        assert_eq!(at(&app), "#1069");

        // Every stop is reachable by ↓ alone — the property that was broken.
        let total = app.stops().len();
        for _ in 0..total {
            app.handle_key(key(KeyCode::Up)); // back to the very first stop
        }
        let mut seen = vec![at(&app)];
        for _ in 1..total {
            app.handle_key(key(KeyCode::Down));
            seen.push(at(&app));
        }
        assert_eq!(
            seen,
            vec![
                "project blooop/bencher",
                "map #1064",
                "#1069",
                "#1070",
                "#1071",
                "#1072"
            ],
            "↓ walked the whole tree"
        );
    }

    #[test]
    fn the_done_group_is_selectable_and_right_left_open_and_shut_it() {
        let mut app = fixture_app();
        // Walk to the group line: it is an ordinary stop on the same axis.
        while at(&app) != "Done" {
            app.handle_key(key(KeyCode::Down));
        }
        assert!(app.cursor_ticket().is_none(), "a group is not a ticket");
        // Shut: the done ticket is not on screen at all.
        assert!(!app.visible().iter().any(|r| app.ticket(r).number == 2));

        app.handle_key(key(KeyCode::Right));
        assert_eq!(at(&app), "Done", "→ opens it; the cursor stays on the line");
        assert!(
            app.visible().iter().any(|r| app.ticket(r).number == 2),
            "the done ticket is on screen now"
        );
        // And it is reachable: → again steps into the rows it holds.
        app.handle_key(key(KeyCode::Right));
        assert_eq!(at(&app), "#2");
        app.handle_key(key(KeyCode::Left));
        assert_eq!(at(&app), "Done");
        app.handle_key(key(KeyCode::Left));
        assert_eq!(at(&app), "Done", "← shuts it, cursor still on the line");
        assert!(!app.visible().iter().any(|r| app.ticket(r).number == 2));
    }

    #[test]
    fn enter_on_a_group_folds_it_rather_than_claiming_nothing_is_selected() {
        let mut app = launchable_app();
        while at(&app) != "Done" {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.notice, None, "no 'nothing selected' — something is");
        assert!(app.visible().iter().any(|r| app.ticket(r).number == 2));
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.visible().iter().any(|r| app.ticket(r).number == 2));
    }

    #[test]
    fn an_expansion_survives_a_refetch() {
        // Keyed by (map, kind), so swapping the clusters underneath keeps the
        // group open — and keeps the cursor on the group line.
        let mut app = fixture_app();
        while at(&app) != "Done" {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Right));
        let same = app.clusters.clone();
        app.replace_clusters(same);
        assert_eq!(at(&app), "Done");
        assert!(app.visible().iter().any(|r| app.ticket(r).number == 2));
    }

    #[test]
    fn tab_toggles_the_lens_and_the_cursor_stays_on_its_ticket() {
        let mut app = fixture_app();
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Right)); // into its subtree: #7
        assert_eq!(at(&app), "#7");
        assert_eq!(app.screen(), Screen::Structured(Lens::Leverage));

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.screen(), Screen::Structured(Lens::Forest));
        // The forest shows done #2 as a row and reorders — the cursor follows
        // its ticket, not its old position.
        assert_eq!(at(&app), "#7");
        assert_eq!(app.visible().len(), 4, "the forest is total");
        assert_eq!(
            app.stops().len(),
            6,
            "and holds nothing back to open — the two extra stops are the \
             project row and this cluster's header"
        );

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.screen(), Screen::Structured(Lens::Leverage));
        assert_eq!(at(&app), "#7");
    }

    #[test]
    fn a_live_query_sifts_the_lens_and_clearing_it_restores_it() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Tab)); // forest
        type_str(&mut app, "bread");
        assert_eq!(
            app.screen(),
            Screen::Sifted {
                lens: Lens::Forest,
                query: "bread"
            },
            "a query sifts the lens it is typed over, it does not replace it"
        );
        assert_eq!(app.visible().len(), 1);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(
            app.screen(),
            Screen::Structured(Lens::Forest),
            "esc clears the query back to the lens it sifted"
        );
    }

    #[test]
    fn esc_clears_query_first_then_quits() {
        let mut app = fixture_app();
        type_str(&mut app, "bread");
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Continue);
        assert!(app.query.is_empty());
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Quit);
    }

    #[test]
    fn q_quits_only_on_empty_query() {
        let mut app = fixture_app();
        type_str(&mut app, "bre");
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Outcome::Continue);
        assert_eq!(app.query, "breq"); // q typed, not quit
        app.query.clear();
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Outcome::Quit);
    }

    /// The fixture app plus launch inputs: one checkout of wayfinder, two of
    /// dotfiles.
    fn launchable_app() -> App {
        let checkout = |path: &str, repo: &str| {
            Checkout::new(std::path::PathBuf::from(path), repo.to_string())
        };
        fixture_app().with_checkouts(vec![
            checkout("/data/proj/wayfinder", "blooop/wayfinder"),
            checkout("/data/k1/dotfiles", "blooop/dotfiles"),
            checkout("/data/k2/dotfiles", "blooop/dotfiles"),
        ])
    }

    /// The same, standing on the dotfiles project — the repo with two
    /// checkouts, and so the only one whose launch reaches the which-checkout
    /// picker. Its ticket is on *its* screen, not wayfinder's.
    fn two_checkout_app() -> App {
        let mut app = launchable_app();
        app.enter("blooop/dotfiles");
        app
    }

    #[test]
    fn enter_opens_the_launch_picker_and_a_second_enter_launches() {
        // The two-step (#62): the first enter stages the launch — nothing
        // execs yet — and enter on the default row is the interactive launch.
        let mut app = launchable_app();
        // wayfinder#6, whose repo has one checkout.
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch {
                staged,
                candidate,
                agent,
                steer,
            } => {
                assert_eq!(
                    staged.route(Mode::Interactive),
                    Some(Route::Wayfinder),
                    "a task is a decision node"
                );
                assert_eq!(staged.key(), "#6");
                assert_eq!(
                    staged.title(),
                    "Re-entry breadcrumbs",
                    "the picker names it"
                );
                assert_eq!(
                    *candidate,
                    Candidate::Launch {
                        mode: Mode::Interactive,
                        route: Route::Wayfinder
                    },
                    "the picker opens on the default"
                );
                assert_eq!(*agent, Agent::Claude, "the picker opens on Claude");
                assert_eq!(steer, "", "with nothing steering it");
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.key(), "wayfinder#6");
        assert_eq!(launch.cwd(), std::path::Path::new("/data/proj/wayfinder"));
        // The map issue is the cluster's — not a per-repo lookup.
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf 1 6 ctx: …"
        );
        assert_eq!(app.overlay, Overlay::None, "one candidate must not prompt");
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("wayfinder#6 in /data/proj/wayfinder"));
    }

    /// A staged launch of the fixture's #6, for the prewarm-claim tests.
    fn staged_six(app: &App) -> Staged {
        Staged::ticket(
            app.clusters
                .values()
                .flat_map(|m| &m.tickets)
                .find(|t| t.number == 6)
                .expect("#6 is in the fixture"),
            &MapRef::new(&MapId::new("blooop/wayfinder", 1), "Map: wf"),
            crate::model::Stage::Ready,
        )
        .expect("ready is launchable")
    }

    #[test]
    fn nothing_is_claimed_or_recorded_while_the_prewarm_is_off() {
        // Off is the default, and it has to mean *nothing happens*: staging
        // is a keystroke, and the human has not committed to a container, a
        // clone or a branch yet. Recording under a disabled flag would also
        // mean a session that turns it on mid-flight never warms the nodes it
        // had already looked at.
        let mut app = fixture_app();
        let staged = staged_six(&app);
        assert_eq!(app.warm(&staged, false), None);
        assert!(app.prewarmed.is_empty());
        // ...and enabling afterwards still gets its turn.
        assert!(app.warm(&staged, true).is_some());
    }

    #[test]
    fn a_node_is_claimed_once_however_often_it_is_staged() {
        // Backing out of the launch picker and coming back is ordinary use;
        // each visit must not add another `dl up` to the pile.
        let mut app = fixture_app();
        let staged = staged_six(&app);
        assert!(app.warm(&staged, true).is_some(), "the first staging warms");
        for _ in 0..3 {
            assert_eq!(
                app.warm(&staged, true),
                None,
                "a re-stage must not warm again"
            );
        }
        assert_eq!(app.prewarmed.len(), 1);
    }

    #[test]
    fn distinct_nodes_each_get_their_own_claim() {
        // The dedup is per node, not a one-shot latch: staging two tickets in
        // one session must warm both, since they are two workspaces.
        let mut app = fixture_app();
        let six = staged_six(&app);
        let map = Staged::map(&MapRef::new(&MapId::new("blooop/wayfinder", 1), "a map"));
        assert_ne!(six.node_workspace(), map.node_workspace());
        assert!(app.warm(&six, true).is_some());
        assert!(app.warm(&map, true).is_some());
        assert_eq!(app.prewarmed.len(), 2);
    }

    #[test]
    fn staging_a_host_launch_spawns_nothing_and_stamps_nothing() {
        // Even claimed, a launch with no container to start plans no command:
        // the fixture's checkout paths do not exist, so no candidate can
        // declare a devcontainer, which is exactly the host case.
        let mut app = fixture_app().with_checkouts(vec![Checkout::new(
            std::path::PathBuf::from("/data/proj/wayfinder"),
            "blooop/wayfinder".to_string(),
        )]);
        let staged = staged_six(&app);
        assert_eq!(launch::prewarm(&app.checkouts, &staged), None);
        // And what staging records says so: claimed, so a re-stage does not
        // re-walk the checkouts, but with no instant to hand anyone — a stamp
        // here would name a `dl up` that was never fired (#160).
        assert_eq!(app.warm(&staged, true), Some(Prewarmed::Nothing));
    }

    #[test]
    fn a_launch_carries_the_instant_its_own_workspace_was_warmed() {
        // The stamp is a fact about this node's container, so it is looked up
        // by the workspace the launch is about to attach to — the same name
        // the prewarm used, which is what makes the two halves meet (#160).
        let mut app = launchable_app();
        let fired = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_755_194_030);
        let workspace = staged_six(&app)
            .node_workspace()
            .expect("a ticket names a workspace");
        app.prewarmed.insert(workspace, Prewarmed::At(fired));
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(app.prewarm_fired(&launch), Some(fired));
    }

    #[test]
    fn a_launch_of_a_node_nobody_warmed_carries_no_instant() {
        // Two ways to have nothing to hand on, and both must answer the same:
        // a session that warmed a *different* node, and one that warmed this
        // node's workspace but fired nothing into it.
        let mut app = launchable_app();
        let workspace = staged_six(&app)
            .node_workspace()
            .expect("a ticket names a workspace");
        app.prewarmed.insert(
            "blooop/wayfinder@wayfinder/wayfinder-999".to_string(),
            Prewarmed::At(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_755_194_030)),
        );
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(app.prewarm_fired(&launch), None);
        app.prewarmed.insert(workspace, Prewarmed::Nothing);
        assert_eq!(app.prewarm_fired(&launch), None);
    }

    #[test]
    fn the_same_ticket_on_two_maps_launches_with_the_cluster_it_was_picked_in() {
        // One repo, two open maps, both listing ticket #6: the row's cluster —
        // not the repo — decides `/wf`'s map argument.
        let mut clusters = BTreeMap::new();
        for map_number in [1u64, 47] {
            clusters.insert(
                MapId::new("blooop/wayfinder", map_number),
                Map {
                    title: format!("Map: {map_number}"),
                    last_activity: None,
                    tickets: vec![ticket(
                        "blooop/wayfinder",
                        6,
                        "Shared ticket",
                        true,
                        false,
                        vec![],
                    )],
                },
            );
        }
        let mut app = app_on("blooop/wayfinder", clusters).with_checkouts(vec![Checkout::new(
            std::path::PathBuf::from("/data/proj/wayfinder"),
            "blooop/wayfinder".to_string(),
        )]);
        // Both clusters hold a #6; the second one is map #47's copy.
        go_to(&mut app, "map #47");
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Enter)); // stage it
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf 47 6 ctx: …"
        );
    }

    #[test]
    fn ctrl_a_no_longer_launches_anything() {
        // Unattended work is another terminal session, not a keystroke here
        // (#26): `ctrl-a` must be inert rather than quietly typing an `a`.
        let mut app = launchable_app();
        assert_eq!(app.handle_key(ctrl('a')), Outcome::Continue);
        assert_eq!(app.query, "");
        assert_eq!(app.notice, None);
    }

    #[test]
    fn several_checkouts_open_the_picker_and_enter_launches_the_pick() {
        let mut app = two_checkout_app();
        // dotfiles#103 — a repo with two checkouts. The first enter stages the
        // launch; the second resolves it to the picker.
        go_to(&mut app, "#103");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickCheckout { launches, cursor } => {
                assert_eq!(launches.len(), 2);
                assert_eq!(*cursor, 0);
            }
            other @ (Overlay::None | Overlay::PickLaunch { .. }) => {
                panic!("expected the picker, got {other:?}")
            }
        }
        // The picker owns every key: typing must not leak into the query.
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.query, "");
        app.handle_key(key(KeyCode::Down));
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.cwd(), std::path::Path::new("/data/k2/dotfiles"));
        assert_eq!(launch.key(), "dotfiles#103");
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn the_picker_clamps_and_esc_cancels_it() {
        let mut app = two_checkout_app();
        go_to(&mut app, "#103");
        app.handle_key(key(KeyCode::Enter)); // dotfiles#103: stage the launch
        app.handle_key(key(KeyCode::Enter)); // resolve — two checkouts: picker
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Down));
        }
        match &app.overlay {
            Overlay::PickCheckout { cursor, .. } => assert_eq!(*cursor, 1),
            other @ (Overlay::None | Overlay::PickLaunch { .. }) => {
                panic!("expected the picker, got {other:?}")
            }
        }
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Continue);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.notice.as_deref(), Some("launch cancelled"));
        // Esc after cancelling behaves normally again (empty query → quit).
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Quit);
    }

    #[test]
    fn a_repo_with_no_checkout_says_so_instead_of_launching() {
        // No checkouts registered: every ticket is unlaunchable — and the one
        // reason left is the missing checkout, because a row's map is its
        // cluster and cannot be missing. The line still opens (the route is a
        // fact about the ticket); the *resolution* is what has nowhere to run.
        let mut app = fixture_app();
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let notice = app.notice.as_deref().unwrap();
        assert!(
            notice.contains("no registered checkout"),
            "notice: {notice}"
        );
    }

    #[test]
    fn the_picker_keeps_the_agent_and_candidate_axes_independent() {
        let mut app = launchable_app();
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Enter));
        let choice = |app: &App| match &app.overlay {
            Overlay::PickLaunch {
                agent, candidate, ..
            } => (*agent, *candidate),
            other => panic!("expected the launch picker, got {other:?}"),
        };
        let launch = |mode| Candidate::Launch {
            mode,
            route: match mode {
                Mode::Interactive => Route::Wayfinder,
                Mode::Mid => Route::WayfinderMid,
                Mode::Auto => Route::WayfinderAuto,
                Mode::Plain => Route::Plain,
            },
        };
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Interactive)));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Plain)));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Auto)));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Mid)));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Interactive)));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Mid)));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Auto)));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Plain)));
        app.handle_key(key(KeyCode::Right));
        assert_eq!(choice(&app), (Agent::Codex, launch(Mode::Plain)));
        app.handle_key(key(KeyCode::Left));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Plain)));
        type_str(&mut app, "keep me");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(choice(&app), (Agent::Claude, launch(Mode::Interactive)));
        match &app.overlay {
            Overlay::PickLaunch {
                agent,
                candidate,
                steer,
                ..
            } => {
                assert_eq!(*agent, Agent::Claude);
                assert_eq!(*candidate, launch(Mode::Interactive));
                assert_eq!(steer, "keep me");
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Right));
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent(), Agent::Codex);
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().expect("one prompt")),
            "$wf 1 6 ctx: … steer: keep me"
        );
    }

    #[test]
    fn launch_picker_typing_steers_and_esc_restores_the_list() {
        // The picker owns every printable key: nothing leaks into the query.
        // Esc backs out with the query and the cursor exactly as they were.
        let mut app = launchable_app();
        type_str(&mut app, "bread"); // flattens to wayfinder#6
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(key(KeyCode::Enter));
        type_str(&mut app, "half a thought");
        app.handle_key(key(KeyCode::Backspace));
        match &app.overlay {
            Overlay::PickLaunch { steer, .. } => assert_eq!(steer, "half a though"),
            other => panic!("expected the launch picker, got {other:?}"),
        }
        assert_eq!(app.query, "bread", "typing stayed out of the query");

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.query, "bread", "esc kept the query");
        assert_eq!(
            app.cursor_ticket().unwrap().number,
            6,
            "esc kept the cursor"
        );
        // And esc means *back to the list*, never quit-from-the-picker: the next
        // esc clears the query, the one after quits — the ordinary ladder.
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Continue);
        assert!(app.query.is_empty());
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Quit);
    }

    #[test]
    fn picking_auto_in_the_overlay_launches_the_manager_with_steering() {
        // The acceptance shape (#96), through the picker: enter → move to the
        // `auto` row → type the steer → enter produces the manager skill
        // carrying `steer: something`. Two downs, because `mid` sits between
        // the default and `auto` on the one axis the rows are ordered by.
        let mut app = launchable_app();
        go_to(&mut app, "#6"); // wayfinder#6, one checkout
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        type_str(&mut app, "something");
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-auto 1 6 ctx: … steer: something"
        );
    }

    #[test]
    fn the_picked_mode_survives_the_checkout_picker() {
        // Two checkouts of dotfiles: the mode is settled in the launch
        // overlay, the checkout picker only answers *where* — the pick must
        // not lose the `auto`.
        let mut app = two_checkout_app();
        go_to(&mut app, "#103");
        app.handle_key(key(KeyCode::Enter)); // dotfiles#103: stage
        app.handle_key(key(KeyCode::Down)); // past `mid`…
        app.handle_key(key(KeyCode::Down)); // …to the auto row
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert!(matches!(app.overlay, Overlay::PickCheckout { .. }));
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-auto 4 103 ctx: …"
        );
    }

    #[test]
    fn a_staged_launch_survives_a_refetch_moving_its_ticket() {
        // The picker stays up while background fetches are still landing (#27),
        // and each arrival swaps the clusters underneath it. A staged launch
        // is snapshotted index-free, so the picker keeps naming — and launching
        // — the ticket it was opened on, wherever that ticket now sits.
        let staged = || {
            let mut app = launchable_app();
            go_to(&mut app, "#6");
            app.handle_key(key(KeyCode::Enter));
            app.handle_key(key(KeyCode::Down)); // past `mid`…
            app.handle_key(key(KeyCode::Down)); // …to the auto row
            app
        };
        let wf = MapId::new("blooop/wayfinder", 1);

        // Reordered: #6's old index now names #9.
        let mut app = staged();
        let mut reordered = app.clusters.clone();
        reordered.get_mut(&wf).expect("the map").tickets = vec![
            ticket(
                "blooop/wayfinder",
                6,
                "Re-entry breadcrumbs",
                true,
                false,
                vec![],
            ),
            ticket(
                "blooop/wayfinder",
                9,
                "Main screen design",
                true,
                true,
                vec![],
            ),
        ];
        app.replace_clusters(reordered);
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-auto 1 6 ctx: …",
            "the staged ticket, not whatever landed at its old index"
        );

        // Shorter: the old index is off the end entirely, and the map the row
        // was picked in has gone. Neither may panic, and the launch still
        // names the ticket the human chose.
        let mut app = staged();
        let mut shrunk = BTreeMap::new();
        shrunk.insert(
            MapId::new("blooop/dotfiles", 4),
            app.clusters
                .get(&MapId::new("blooop/dotfiles", 4))
                .expect("the dotfiles map")
                .clone(),
        );
        app.replace_clusters(shrunk);
        match &app.overlay {
            Overlay::PickLaunch { staged, .. } => assert_eq!(staged.key(), "#6"),
            other => panic!("expected the launch picker, got {other:?}"),
        }
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-auto 1 6 ctx: …"
        );
    }

    /// The launchable app, plus a conversation left on `number` by a previous
    /// launch in the wayfinder checkout.
    fn app_resuming(number: u64, agent: Agent) -> App {
        launchable_app().with_sessions(vec![Session::new(
            PROJECT.to_string(),
            number,
            agent,
            std::path::PathBuf::from("/data/proj/wayfinder"),
            Isolation::Host,
        )])
    }

    #[test]
    fn a_ticket_you_were_working_stages_with_the_way_back_under_the_cursor() {
        // The point of the whole feature, read off one keystroke: come back to
        // a ticket you had an agent on and the picker opens *on* the resume
        // row, so `enter enter` rejoins instead of starting a second session
        // beside the first.
        let mut app = app_resuming(6, Agent::Claude);
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch {
                staged, candidate, ..
            } => {
                assert_eq!(staged.key(), "#6");
                assert_eq!(
                    *candidate,
                    Candidate::Resume {
                        agent: Agent::Claude
                    }
                );
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
    }

    #[test]
    fn a_ticket_with_no_session_of_its_own_stages_exactly_as_it_always_did() {
        // The record is per node, so the neighbour of a resumable ticket is
        // not resumable — the row would otherwise rejoin the wrong work, which
        // is the one mistake this feature could make that costs real time.
        let mut app = app_resuming(9, Agent::Claude);
        go_to(&mut app, "#6");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch { candidate, .. } => assert_eq!(
                *candidate,
                Candidate::Launch {
                    mode: Mode::Interactive,
                    route: Route::Wayfinder,
                }
            ),
            other => panic!("expected the launch picker, got {other:?}"),
        }
    }

    #[test]
    fn resuming_execs_into_the_tree_the_conversation_is_in_without_asking() {
        // Two enters and nothing else. The dotfiles repo has two registered
        // checkouts, so a *fresh* launch of one of its tickets would stop to
        // ask which tree; a resume never does, because the conversation exists
        // in exactly one of them and the record says which.
        let mut app = launchable_app().with_sessions(vec![Session::new(
            "blooop/dotfiles".to_string(),
            103,
            Agent::Claude,
            std::path::PathBuf::from("/data/k2/dotfiles"),
            Isolation::Host,
        )]);
        app.enter("blooop/dotfiles");
        go_to(&mut app, "#103");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.cwd(), std::path::Path::new("/data/k2/dotfiles"));
        assert!(
            launch.agent_argv().contains(&"--continue".to_string()),
            "the exec must be the agent's own way back: {:?}",
            launch.agent_argv()
        );
        assert!(
            launch.describe().starts_with("resume dotfiles#103"),
            "the notice says it is going back, not starting: {}",
            launch.describe()
        );
    }

    #[test]
    fn the_agent_keys_are_dead_on_the_resume_row_because_the_record_decides() {
        // A Claude conversation cannot be rejoined by Codex. The picker's
        // horizontal axis is a real choice on every other row, so on this one
        // it has to visibly do nothing rather than change a title over a row
        // that will run the other CLI regardless.
        let mut app = app_resuming(6, Agent::Claude);
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Right));
        match &app.overlay {
            Overlay::PickLaunch {
                agent, candidate, ..
            } => {
                assert_eq!(*agent, Agent::Claude, "the arrow must not have moved it");
                assert_eq!(candidate.agent(*agent), Agent::Claude);
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        // And on any other row it is alive again, so nothing was disabled but
        // the one row that cannot honour it.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        match &app.overlay {
            Overlay::PickLaunch { agent, .. } => assert_eq!(*agent, Agent::Codex),
            other => panic!("expected the launch picker, got {other:?}"),
        }
    }

    #[test]
    fn a_map_you_have_charted_before_can_be_rejoined_too() {
        // The record is keyed by node, and a map is a node (#96) — charting
        // sessions are exactly the long conversations worth coming back to.
        let mut app = app_resuming(1, Agent::Codex);
        go_to(&mut app, "map #1");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch { candidate, .. } => assert_eq!(
                *candidate,
                Candidate::Resume {
                    agent: Agent::Codex
                }
            ),
            other => panic!("expected the launch picker, got {other:?}"),
        }
    }

    #[test]
    fn a_project_row_offers_no_resume_however_many_sessions_the_repo_has() {
        // Creation names work that does not exist yet. The repo having live
        // conversations on three tickets says nothing about a fourth that has
        // not been filed.
        let mut app = app_resuming(6, Agent::Claude);
        go_to(&mut app, &format!("project {PROJECT}"));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch {
                staged, candidate, ..
            } => {
                assert_eq!(*candidate, Candidate::Create(CreationKind::Task));
                assert!(staged
                    .candidates()
                    .iter()
                    .all(|c| !matches!(c, Candidate::Resume { .. })));
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_a_cluster_header_stages_the_whole_map() {
        // #96's headline: the cursor can land on a map, and launching one runs
        // the wayfinder skill on the map alone — no ticket argument, because
        // there is no ticket. Interactive charts it with the human; `auto`
        // hands the whole map to the manager.
        let mut app = launchable_app();
        go_to(&mut app, "map #1");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch { staged, .. } => {
                assert_eq!(staged.key(), "#1");
                assert_eq!(staged.title(), "Map: wf", "the picker names the map");
                assert_eq!(staged.route(Mode::Interactive), Some(Route::Wayfinder));
                assert_eq!(staged.route(Mode::Mid), Some(Route::WayfinderMid));
                assert_eq!(staged.route(Mode::Auto), Some(Route::WayfinderAuto));
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.key(), "wayfinder#1");
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf 1 ctx: …"
        );

        let mut app = launchable_app();
        go_to(&mut app, "map #1");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Down)); // past `mid`…
        app.handle_key(key(KeyCode::Down)); // …to the auto row
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-auto 1 ctx: …"
        );
    }

    /// An app on a registered repo that has no open map — what `wf` opened
    /// inside a fresh checkout is looking at.
    fn mapless_app() -> App {
        let mut app = App::new(BTreeMap::new()).with_checkouts(vec![Checkout::new(
            std::path::PathBuf::from("/data/proj/newthing"),
            "blooop/newthing".to_string(),
        )]);
        app.enter("blooop/newthing");
        app
    }

    #[test]
    fn a_project_screen_leads_with_its_own_row_even_with_no_map() {
        // The row is a place to stand, not a report on what was found: this
        // screen used to render nothing at all, which is where the first map
        // of a repo had no way in. It is a stop, so the cursor can name it and
        // `enter` can act on it — and it is the *first* stop, so an untouched
        // cursor is already on it.
        let app = mapless_app();
        assert_eq!(
            app.stops()
                .iter()
                .map(|at| at.stop.clone())
                .collect::<Vec<_>>(),
            vec![Stop::Project("blooop/newthing".to_string())]
        );
        assert_eq!(
            app.cursor_stop(),
            Some(Stop::Project("blooop/newthing".to_string()))
        );
    }

    #[test]
    fn the_project_row_leads_a_screen_that_has_maps_too() {
        // Not only the empty case: the row is the repo-level stop wherever the
        // screen is, and the clusters follow it.
        let app = fixture_app();
        let stops: Vec<Stop> = app.stops().iter().map(|at| at.stop.clone()).collect();
        assert_eq!(
            stops.first(),
            Some(&Stop::Project("blooop/wayfinder".to_string()))
        );
        assert!(
            stops.iter().any(|s| matches!(s, Stop::Map(_))),
            "the clusters still follow it: {stops:?}"
        );
    }

    #[test]
    fn a_mapless_repo_offers_creation_and_nothing_to_launch() {
        // There is no node here, so there is nothing to launch: the picker is
        // the creation rows alone. A launch row would name a skill with no
        // argument to give it.
        let mut app = mapless_app();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch {
                staged, candidate, ..
            } => {
                assert_eq!(
                    staged.candidates(),
                    vec![
                        Candidate::Create(CreationKind::Task),
                        Candidate::Create(CreationKind::Map),
                        Candidate::Create(CreationKind::MapMid),
                        Candidate::Create(CreationKind::MapAuto),
                    ]
                );
                assert_eq!(*candidate, Candidate::Create(CreationKind::Task));
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        // And it charts: the first map of this repo, from the door that had
        // nothing behind it before.
        app.handle_key(key(KeyCode::Down)); // onto `new map`
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/wf");
        assert_eq!(launch.cwd(), std::path::Path::new("/data/proj/newthing"));
    }

    #[test]
    fn the_project_row_is_a_stop_before_the_load_lands() {
        // The old empty-state door had to wait for the search, because a repo
        // whose maps were still in flight looked exactly like one that had
        // none and the door was the *answer* to having none. This row is not
        // an answer, so it does not wait: it is the same stop, meaning the
        // same thing, on the first frame. Which is what lets `wf` in a checkout
        // start a task before a single `gh` call has returned.
        let mut app = mapless_app();
        app.startup = Startup::default();
        assert_eq!(app.stops().len(), 1, "the row is there while searching");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert!(
            matches!(app.overlay, Overlay::PickLaunch { .. }),
            "and enter still opens the creation rows on it"
        );
    }

    #[test]
    fn every_registered_repo_is_a_row_on_the_project_list() {
        // The reversal of #114's "map-less repos stay off the widened screen":
        // one row per project is not permanent furniture on a ticket screen
        // any more, it *is* the top-level screen. A repo with no map is a row
        // there like any other — which is how it is reached from outside.
        // Stamped rather than left to the clock: the order is the assertion,
        // so two checkouts registered in the same second would make it a
        // coin toss.
        let used = |path: &str, repo: &str, at: u64| Checkout {
            path: std::path::PathBuf::from(path),
            repo: repo.to_string(),
            used: Some(at),
        };
        let mut app = fixture_app().with_checkouts(vec![
            used("/data/proj/wayfinder", "blooop/wayfinder", 100),
            used("/data/proj/newthing", "blooop/newthing", 200),
        ]);
        app.level = Level::Projects;
        let stops: Vec<Stop> = app.stops().iter().map(|at| at.stop.clone()).collect();
        assert_eq!(
            stops,
            vec![
                Stop::Project("blooop/newthing".to_string()),
                Stop::Project("blooop/wayfinder".to_string()),
            ],
            "both projects, mapped or not, most recently used first — and no \
             cluster among them"
        );
    }

    #[test]
    fn only_the_project_stop_reaches_the_creation_rows() {
        // Creation is a repo-level act, so the rows exist exactly where the
        // stop is a repo — and nowhere else. A cluster header is a *map*, so
        // its picker walks the modes and wraps, exactly as a ticket's does:
        // the only difference between the two is what they aim at.
        let picked = |app: &App| match &app.overlay {
            Overlay::PickLaunch { candidate, .. } => *candidate,
            other => panic!("expected the launch picker, got {other:?}"),
        };

        let mut app = launchable_app();
        assert_eq!(at(&app), "project blooop/wayfinder");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(picked(&app), Candidate::Create(CreationKind::Task));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(picked(&app), Candidate::Create(CreationKind::Map));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(picked(&app), Candidate::Create(CreationKind::MapMid));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(picked(&app), Candidate::Create(CreationKind::MapAuto));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(
            picked(&app),
            Candidate::Create(CreationKind::Task),
            "and wraps: there is nothing else on a project's picker"
        );

        // A header and a ticket both walk only the modes: a lap of `Mode::all`
        // lands back on the default on either.
        for stop in ["map #1", "#6"] {
            let mut app = launchable_app();
            go_to(&mut app, stop);
            app.handle_key(key(KeyCode::Enter));
            for _ in 0..Mode::all().len() {
                app.handle_key(key(KeyCode::Down));
            }
            assert!(
                matches!(picked(&app), Candidate::Launch { .. }),
                "no creation rows on {stop}"
            );
        }
    }

    #[test]
    fn a_new_task_launches_wf_one_and_refuses_an_empty_task() {
        let mut app = launchable_app();
        // The project row is where an untouched cursor already is, and `new
        // task` is the row its picker opens on: `enter`, type, `enter`.
        app.handle_key(key(KeyCode::Enter));
        // Enter with nothing typed refuses on the count line — the overlay
        // stays up, as a done or blocked node already refuses.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert!(
            matches!(app.overlay, Overlay::PickLaunch { .. }),
            "the picker stays up to take the task"
        );
        assert!(
            app.notice.as_deref().unwrap().contains("task"),
            "{:?}",
            app.notice
        );
        // With the task typed, enter execs /wf-one with it verbatim.
        type_str(&mut app, "wire the exporter");
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch.agent_argv().last().unwrap(),
            "/wf-one wire the exporter"
        );
    }

    #[test]
    fn a_new_map_launches_the_charting_skill_with_the_text_as_its_seed() {
        // The seed is optional: bare `/wf` charts from nothing.
        let mut app = launchable_app();
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Down)); // onto `new map`
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/wf");

        // And seeded, alone: the auto charting row takes the idea verbatim.
        let mut app = launchable_app();
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Up)); // wrap straight onto `new map, auto`
        type_str(&mut app, "a caching layer");
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch.agent_argv().last().unwrap(),
            "/wf-auto a caching layer"
        );
    }

    #[test]
    fn enter_on_a_done_or_blocked_node_is_a_notice_not_a_launch_picker() {
        let mut app = launchable_app();
        // Blocked: #7 hangs under #6 as context.
        go_to(&mut app, "#6");
        app.handle_key(key(KeyCode::Right)); // into #7
        assert_eq!(at(&app), "#7");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.overlay, Overlay::None, "no picker on a blocked node");
        assert!(
            app.notice.as_deref().unwrap().contains("blocked"),
            "{:?}",
            app.notice
        );
        // Done: #2 sits behind the Done group.
        while at(&app) != "Done" {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Right)); // open the group
        app.handle_key(key(KeyCode::Right)); // onto #2
        assert_eq!(at(&app), "#2");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.overlay, Overlay::None, "no picker on a done node");
        assert!(
            app.notice.as_deref().unwrap().contains("done"),
            "{:?}",
            app.notice
        );
    }

    #[test]
    fn a_build_node_routes_by_its_stage() {
        // One build ticket, staged by its PR: in review → /wf-review; the same
        // ticket with no PR evidence is ready → /wf-tdd.
        let build_app = |prs: Vec<PrLink>| -> App {
            let mut t = ticket(
                "blooop/wayfinder",
                65,
                "Author the /wf-tdd skill",
                true,
                false,
                vec![],
            );
            t.ticket_type = TicketType::Build;
            t.prs = prs;
            let mut clusters = BTreeMap::new();
            clusters.insert(
                MapId::new("blooop/wayfinder", 59),
                Map {
                    title: "Map: the spine".to_string(),
                    last_activity: None,
                    tickets: vec![t],
                },
            );
            app_on("blooop/wayfinder", clusters).with_checkouts(vec![Checkout::new(
                std::path::PathBuf::from("/data/proj/wayfinder"),
                "blooop/wayfinder".to_string(),
            )])
        };

        let mut ready = build_app(vec![]);
        go_to(&mut ready, "#65");
        ready.handle_key(key(KeyCode::Enter));
        match &ready.overlay {
            Overlay::PickLaunch { staged, .. } => {
                assert_eq!(staged.route(Mode::Interactive), Some(Route::Tdd));
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        let launch = match ready.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-tdd 65 ctx: …"
        );

        let mut in_review = build_app(vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }]);
        go_to(&mut in_review, "#65");
        in_review.handle_key(key(KeyCode::Enter));
        match &in_review.overlay {
            Overlay::PickLaunch { staged, .. } => {
                assert_eq!(staged.route(Mode::Interactive), Some(Route::Review));
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        let launch = match in_review.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch::eliding_ctx(launch.agent_argv().last().unwrap()),
            "/wf-review 65 ctx: …"
        );

        // A merged PR with nothing open means done — stage, not ticket state,
        // is what refuses the launch.
        let mut done = build_app(vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Merged,
        }]);
        go_to(&mut done, "#65");
        assert_eq!(done.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(done.overlay, Overlay::None);
        assert!(done.notice.as_deref().unwrap().contains("done"));
    }

    #[test]
    fn launching_with_nothing_visible_is_a_notice_not_a_panic() {
        let mut app = launchable_app();
        type_str(&mut app, "zzzz");
        assert!(app.cursor_ticket().is_none());
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.notice.as_deref(), Some("nothing selected"));
    }

    #[test]
    fn the_retired_chords_do_nothing_at_all() {
        // `ctrl-f` and `ctrl-g` are unbound, not merely undocumented: focusing
        // a project is entering it and widening is `←` out of it. Pressing
        // either must not change the level, move the cursor, or type a letter
        // into the query.
        let mut app = fixture_app();
        go_to(&mut app, "#6");
        let before = (app.level.clone(), app.cursor_pos(), app.query.clone());
        for chord in ['f', 'g'] {
            assert_eq!(app.handle_key(ctrl(chord)), Outcome::Continue);
            assert_eq!(
                (app.level.clone(), app.cursor_pos(), app.query.clone()),
                before,
                "ctrl-{chord} did something"
            );
        }
    }

    #[test]
    fn entering_a_project_keeps_every_open_map_of_it_and_nobody_elses() {
        // Two maps on one repo: a project is a repo, not a map, so entering it
        // shows both — and the other repo's map is on another screen entirely.
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                last_activity: None,
                tickets: vec![ticket("blooop/wayfinder", 6, "t6", true, false, vec![])],
            },
        );
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: selection view".to_string(),
                last_activity: None,
                tickets: vec![ticket("blooop/wayfinder", 50, "t50", true, false, vec![])],
            },
        );
        clusters.insert(
            MapId::new("blooop/dotfiles", 4),
            Map {
                title: "Map: dotfiles".to_string(),
                last_activity: None,
                tickets: vec![ticket("blooop/dotfiles", 103, "t103", true, false, vec![])],
            },
        );
        let app = app_on("blooop/wayfinder", clusters);
        let visible = app.visible();
        assert_eq!(visible.len(), 2, "both wayfinder maps are on this screen");
        assert!(visible.iter().all(|row| row.map.repo == "blooop/wayfinder"));
    }

    #[test]
    fn a_project_screen_separates_a_fork_from_its_upstream() {
        // Two repos sharing a short name: identity is the slug, so one
        // project's screen must not drag the other's rows in.
        let mut clusters = BTreeMap::new();
        for owner in ["blooop", "upstream"] {
            clusters.insert(
                MapId::new(format!("{owner}/dotfiles"), 1),
                Map {
                    title: "Map: dotfiles".to_string(),
                    last_activity: None,
                    tickets: vec![ticket(
                        &format!("{owner}/dotfiles"),
                        5,
                        "Prune legacy bash aliases",
                        true,
                        false,
                        vec![],
                    )],
                },
            );
        }
        let app = app_on("upstream/dotfiles", clusters);
        assert_eq!(
            app.visible().len(),
            1,
            "the fork's identically-numbered row must not show"
        );
        assert_eq!(app.visible()[0].map.repo, "upstream/dotfiles");
    }

    #[test]
    fn a_query_on_the_project_list_matches_slugs_and_keeps_no_tickets() {
        // One query field, one register per level: at the top it is looking for
        // a project, so it matches the slug and nothing else — a ticket title
        // that would match cannot pull its project's rows onto this screen.
        let mut app = launchable_app();
        app.level = Level::Projects;
        type_str(&mut app, "dotf");
        assert_eq!(
            app.stops()
                .iter()
                .map(|at| at.stop.clone())
                .collect::<Vec<_>>(),
            vec![Stop::Project("blooop/dotfiles".to_string())]
        );
        assert!(app.visible().is_empty(), "no ticket rows on the list");

        // And a query that matches no project empties the list rather than
        // falling back to something.
        app.query.clear();
        type_str(&mut app, "zzzz");
        assert!(app.stops().is_empty());
    }

    #[test]
    fn the_count_line_counts_whatever_the_screen_is_a_list_of() {
        // Tickets on a project's screen, projects on the list. Counting
        // tickets on the list would read `0/0` under nine projects — the same
        // number an empty screen shows — and a query would narrow a count it
        // was not narrowing.
        let mut app = launchable_app();
        assert_eq!(
            app.counts(),
            (3, 4),
            "the wayfinder map's rows, its done one collapsed"
        );

        app.level = Level::Projects;
        assert_eq!(app.counts(), (2, 2), "wayfinder and dotfiles");
        type_str(&mut app, "dotf");
        assert_eq!(app.counts(), (1, 2), "narrowed, out of all of them");
    }

    /// A map with a diamond in its DAG: #14 needs both #6 and #9, and both
    /// are takeable roots, so the leverage lens deliberately draws #14 under
    /// each of them — four rows for three shown tickets (the done #2 is
    /// folded away). The shape the count and cursor claims below are about.
    fn diamond_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new(PROJECT, 1),
            Map {
                title: "Map: diamond".to_string(),
                last_activity: None,
                tickets: vec![
                    ticket(PROJECT, 2, "Choose the stack", false, true, vec![]),
                    ticket(PROJECT, 6, "Re-entry breadcrumbs", true, false, vec![]),
                    ticket(PROJECT, 9, "Main screen design", true, true, vec![]),
                    ticket(PROJECT, 14, "Breadcrumb markers", true, false, vec![6, 9]),
                ],
            },
        );
        app_on(PROJECT, clusters)
    }

    #[test]
    fn a_ticket_a_diamond_draws_twice_is_counted_once() {
        // The count line reads `shown/total` **of tickets** — the same
        // nodes-not-rows discipline the rollups already keep. The diamond
        // renders #14 twice, so counting drawn rows would claim four tickets
        // shown out of four while the done one sits folded off screen.
        let app = diamond_app();
        assert_eq!(
            app.visible().len(),
            4,
            "the lens draws four rows: #14 under both roots"
        );
        assert_eq!(
            app.counts(),
            (3, 4),
            "three distinct tickets shown, of the map's four"
        );
    }

    /// Positions in the stop list of every drawing of ticket `number` — plural
    /// on purpose: on a diamond the same ticket is drawn more than once, and
    /// which drawing the cursor is on is exactly what these tests are about.
    fn drawings_of(app: &App, number: u64) -> Vec<usize> {
        app.stops()
            .iter()
            .enumerate()
            .filter_map(|(i, at)| match &at.stop {
                Stop::Ticket(row) if app.ticket(row).number == number => Some(i),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_cursor_on_the_second_drawing_of_a_diamond_ticket_survives_a_swap() {
        // Identity pinning exists so a refetch never teleports the selection —
        // and an identity that cannot tell the two drawings of #14 apart
        // teleports a cursor from the second drawing to the first on every
        // swap, breaking the guarantee precisely on diamonds.
        let mut app = diamond_app();
        let drawings = drawings_of(&app, 14);
        assert_eq!(drawings.len(), 2, "the diamond draws #14 twice");
        app.cursor = Cursor::Chosen(drawings[1]);
        let same = app.clusters.clone();
        app.replace_clusters(same);
        assert_eq!(
            app.cursor_pos(),
            drawings[1],
            "the drawing that was chosen, not the first one"
        );
    }

    #[test]
    fn a_drawing_that_vanishes_degrades_to_the_ticket_not_the_top() {
        // #9 closes, its root leaves the leverage screen and takes the second
        // drawing of #14 with it — but the ticket is still drawn under #6, so
        // the cursor follows the ticket rather than falling back to a bare
        // position or the top of the list.
        let mut app = diamond_app();
        let drawings = drawings_of(&app, 14);
        app.cursor = Cursor::Chosen(drawings[1]);
        let mut swapped = app.clusters.clone();
        let map = swapped
            .get_mut(&MapId::new(PROJECT, 1))
            .expect("the diamond map");
        map.tickets[2] = ticket(PROJECT, 9, "Main screen design", false, true, vec![]);
        app.replace_clusters(swapped);
        assert_eq!(at(&app), "#14", "the ticket outlives its second drawing");
    }

    #[test]
    fn a_lens_toggle_keeps_the_cursor_on_the_ticket_when_its_drawing_leaves() {
        // The forest lens draws every ticket exactly once, so toggling away
        // from the leverage screen deletes the second drawing. The ticket is
        // still on screen, and the cursor's promise is the ticket.
        let mut app = diamond_app();
        let drawings = drawings_of(&app, 14);
        app.cursor = Cursor::Chosen(drawings[1]);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&app), "#14", "the other lens still shows the ticket");
    }

    #[test]
    fn a_query_puts_the_best_matching_project_first() {
        // The fzf order the clusters already follow: with a query live it is
        // the query's turn to decide, so `enter` after typing runs the thing
        // you were most plainly reaching for rather than whatever you happened
        // to open last.
        let stamped = |path: &str, repo: &str, at: u64| Checkout {
            path: std::path::PathBuf::from(path),
            repo: repo.to_string(),
            used: Some(at),
        };
        let mut app = fixture_app().with_checkouts(vec![
            stamped("/a", "blooop/way", 300),
            stamped("/b", "blooop/wayfinder", 100),
        ]);
        app.level = Level::Projects;
        assert_eq!(at(&app), "project blooop/way", "most recently used first");
        type_str(&mut app, "wayfinder");
        assert_eq!(
            at(&app),
            "project blooop/wayfinder",
            "and the query outranks that"
        );
    }

    #[test]
    fn a_query_hands_enter_back_to_the_best_match() {
        // The creating default must not survive a sift. An untouched cursor
        // means the first stop, so a project row left on a sifted screen would
        // take `enter` on a freshly typed query away from the match and give it
        // to *new task* — the one place where opening on the project is wrong.
        let mut app = launchable_app();
        assert_eq!(at(&app), "project blooop/wayfinder");
        type_str(&mut app, "bread");
        assert_eq!(at(&app), "#6", "the hit, not the project");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickLaunch { staged, .. } => {
                assert!(
                    matches!(staged.candidates()[0], Candidate::Launch { .. }),
                    "a ticket's picker, not the creation rows"
                );
            }
            other => panic!("expected the launch picker, got {other:?}"),
        }
        // Clearing the query puts the row back, and the cursor with it.
        app.overlay = Overlay::None;
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(at(&app), "project blooop/wayfinder");
    }

    #[test]
    fn enter_and_right_both_enter_a_project_and_left_comes_back_to_it() {
        // The whole of the two-level navigation, in one walk. `←` is the back
        // key: from inside a project it climbs to the project row and then out
        // to the list, landing on the project just left rather than at the top.
        let mut app = launchable_app();
        app.level = Level::Projects;
        go_to(&mut app, "project blooop/wayfinder");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.level,
            Level::Project {
                repo: "blooop/wayfinder".to_string()
            }
        );
        // Down into the maps, then `←` all the way back out.
        go_to(&mut app, "#6");
        while app.current_repo().is_some() {
            app.handle_key(key(KeyCode::Left));
        }
        assert_eq!(app.level, Level::Projects);
        assert_eq!(
            app.cursor_stop(),
            Some(Stop::Project("blooop/wayfinder".to_string())),
            "back out onto the project just left"
        );
        // `→` is the same door as `enter`.
        app.handle_key(key(KeyCode::Right));
        assert_eq!(
            app.level,
            Level::Project {
                repo: "blooop/wayfinder".to_string()
            }
        );
    }

    #[test]
    fn replace_clusters_keeps_the_anchor_when_the_ticket_survives() {
        // A map arriving swaps its whole cluster, and the cursor must stay on
        // the ticket it was on rather than on the position it happened to hold.
        let mut app = fixture_app();
        go_to(&mut app, "#6");
        let same = app.clusters.clone();
        app.replace_clusters(same);
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
    }

    #[test]
    fn replace_clusters_does_not_teleport_when_cursor_ticket_vanishes() {
        let mut app = fixture_app();
        go_to(&mut app, "#6"); // position 2: the project row and the header
        let mut smaller = app.clusters.clone();
        smaller
            .get_mut(&MapId::new("blooop/wayfinder", 1))
            .unwrap()
            .tickets
            .retain(|t| t.number != 6);
        app.replace_clusters(smaller);
        // Identity gone: cursor stays at the same position, clamped.
        assert_eq!(app.cursor_pos(), 2);
        assert_eq!(app.cursor_ticket().unwrap().number, 9);
    }

    #[test]
    fn backspace_edits_query_and_refilters() {
        let mut app = fixture_app();
        type_str(&mut app, "breadx");
        assert!(app.visible().is_empty());
        assert!(app.cursor_ticket().is_none());
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.query, "bread");
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
    }
}
