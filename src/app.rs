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

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::launch::{self, Launch, LaunchMode, Staged, Targets};
use crate::model::{stage, Activity, Map, MapId, MapSet, Status, Ticket};
use crate::projects::Checkout;
use crate::refresh::Startup;
use crate::view::{self, Expanded, GroupId, Lens, Plan, Screen, Stop, StopAt};

/// Project scope: everything, or one repo focused via `ctrl-f`. A repo, not a
/// map: focusing where you stand means seeing every open map of that repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Project(String),
}

/// What the event loop should do after a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    /// Force a refetch (`ctrl-r`). The loop performs it and puts the results
    /// back via [`App::replace_clusters`].
    Refresh,
    /// Run this ticket's agent. The last thing `wf` ever does: the loop
    /// returns, the terminal is restored, and the process becomes the agent.
    Launch(Launch),
}

/// A modal layer over the main screen: the staged second step of a launch
/// (#62), or the which-checkout prompt. Either owns every key while up, so no
/// typing leaks into the query behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// The launch line — enter on a launchable node staged this launch, and
    /// the line now collects its mode: empty is interactive, `defer [text]`
    /// defers, anything else steers ([`LaunchMode::parse`]). The route is
    /// already resolved from (type, stage), so the line can *show* where enter
    /// goes — and a line for an unlaunchable node is unrepresentable, because
    /// no `Route` exists to put in it.
    ///
    /// The staged launch is index-free ([`Staged`]) for the same reason the
    /// picker's candidates are complete `Launch`es: a background map arrival
    /// swaps the clusters underneath an open overlay, and a positional [`Row`]
    /// held across that would name a different ticket, or none at all.
    LaunchLine {
        staged: Staged,
        text: String,
    },
    /// Candidates are complete launches, so the pick cannot produce an
    /// inconsistent one.
    PickCheckout {
        launches: Vec<Launch>,
        cursor: usize,
    },
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    Ticket(RowKey),
    Group(GroupId),
}

#[derive(Debug)]
pub struct App {
    /// The clusters on screen: every open map that has arrived, keyed by id.
    /// Render order is *not* this map's key order — it is decided by
    /// [`App::scoped_clusters`], which leads on activity.
    pub clusters: BTreeMap<MapId, Map>,
    /// The maps believed open — the cached seed until the search answers, the
    /// search's answer afterwards. This is what `ctrl-r` refetches.
    pub open_maps: MapSet,
    pub query: String,
    pub scope: Scope,
    /// One-shot status message shown on the count line; cleared on the next
    /// keypress.
    pub notice: Option<String>,
    /// Launch input from the projects cache (#15 handoff): which checkouts
    /// exist on this machine.
    pub checkouts: Vec<Checkout>,
    /// Maps whose last fetch failed — **state, not a message.**
    ///
    /// A failure has to be drawn on every frame, and the one-shot `notice` is
    /// cleared by the very next keypress. Nothing polls any more either, so a
    /// failed fetch is the final word on that map until `ctrl-r`: with only a
    /// notice, one keystroke turns "GitHub is down" into a screen that says
    /// *no projects — run wf inside a checkout to register it*, which is the
    /// exact lie [`crate::refresh::Startup`] exists to prevent for the
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
    pub overlay: Overlay,
    /// The structural screen `tab` toggles (#51). Only the lens is stored;
    /// whether the body is currently *flattened* is derived from the query in
    /// [`App::screen`], so the two can never disagree.
    lens: Lens,
    /// Which collapsible groups the human has opened (#57). Keyed by
    /// [`GroupId`], so an expansion survives a refetch, a query and a lens
    /// toggle — it is a choice about a *map*, not about a frame.
    expanded: Expanded,
    cursor: usize,
}

impl App {
    /// An app over clusters already in hand — so nothing is being waited on.
    pub fn new(clusters: BTreeMap<MapId, Map>) -> Self {
        Self {
            clusters,
            open_maps: MapSet::new(),
            query: String::new(),
            scope: Scope::All,
            notice: None,
            checkouts: Vec::new(),
            failed: BTreeSet::new(),
            startup: Startup::loaded(),
            overlay: Overlay::None,
            lens: Lens::Leverage,
            expanded: Expanded::new(),
            cursor: 0,
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

    fn in_scope(&self, id: &MapId) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Project(repo) => &id.repo == repo,
        }
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
        let mut clusters: Vec<(&MapId, &Map)> = self
            .clusters
            .iter()
            .filter(|(id, _)| self.in_scope(id))
            .collect();
        // `!has_open_work` so `false` (live) sorts before `true` (finished), and
        // `Reverse` so the newest activity leads — with `None` last, since
        // `None < Some` reversed puts the unknown stamps at the end.
        fn key<'a>(
            (id, map): &(&'a MapId, &'a Map),
        ) -> (bool, Reverse<Option<Activity>>, &'a MapId) {
            (!map.has_open_work(), Reverse(map.last_activity), *id)
        }
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

    /// What the body renders this frame (#51): the toggled lens, unless a live
    /// query flattens it. Derived, never stored.
    pub fn screen(&self) -> Screen<'_> {
        if self.query.is_empty() {
            Screen::Structured(self.lens)
        } else {
            Screen::Flattened { query: &self.query }
        }
    }

    /// The body, planned: every line the screen shows, in on-screen order —
    /// what the draw walks and what the cursor navigates, so the two can never
    /// disagree about order.
    pub fn plan(&self) -> Plan {
        view::plan(&self.scoped_clusters(), self.screen(), &self.expanded)
    }

    /// Every cursor stop with its depth, in on-screen order (#57). The cursor
    /// indexes this list; headers and spacers are never stops, but group lines
    /// are — opening one is an action, so it needs naming.
    pub fn stops(&self) -> Vec<StopAt> {
        self.plan().stops()
    }

    /// Ticket rows on screen — what the match count counts. A subset of
    /// [`App::stops`].
    pub fn visible(&self) -> Vec<Row> {
        self.plan().rows()
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

    /// A stop's durable identity: a ticket by (map, number), a group by its
    /// own id — which already names no indices.
    fn stop_key(&self, stop: &Stop) -> StopKey {
        match stop {
            Stop::Ticket(row) => StopKey::Ticket(self.row_key(row)),
            Stop::Group(id) => StopKey::Group(id.clone()),
        }
    }

    /// Cursor position clamped into the stop list.
    pub fn cursor_pos(&self) -> usize {
        self.cursor.min(self.stops().len().saturating_sub(1))
    }

    /// What the cursor is on, if anything is on screen.
    pub fn cursor_stop(&self) -> Option<Stop> {
        self.stops()
            .get(self.cursor_pos())
            .map(|at| at.stop.clone())
    }

    /// The row under the cursor — `None` when the cursor is on a group line,
    /// which is exactly why the two are different types.
    pub fn cursor_row(&self) -> Option<Row> {
        match self.cursor_stop() {
            Some(Stop::Ticket(row)) => Some(row),
            Some(Stop::Group(_)) | None => None,
        }
    }

    /// The ticket under the cursor, if the cursor is on one.
    pub fn cursor_ticket(&self) -> Option<&Ticket> {
        self.cursor_row().map(|row| {
            let map: &Map = &self.clusters[&row.map];
            &map.tickets[row.index]
        })
    }

    /// Which map the cursor is in, whichever kind of stop it is on — what
    /// `ctrl-f` focuses. Every stop belongs to a cluster, so this is total
    /// wherever the cursor can be at all.
    pub fn cursor_map(&self) -> Option<MapId> {
        match self.cursor_stop()? {
            Stop::Ticket(row) => Some(row.map),
            Stop::Group(id) => Some(id.map),
        }
    }

    /// The cursor's stable identity.
    fn cursor_key(&self) -> Option<StopKey> {
        self.cursor_stop().map(|stop| self.stop_key(&stop))
    }

    /// Point the cursor at a specific stop if it is on screen.
    fn point_at(&mut self, key: &StopKey) {
        let pos = self
            .stops()
            .iter()
            .position(|at| &self.stop_key(&at.stop) == key);
        self.cursor = pos.unwrap_or(0);
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
            self.cursor = 0;
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
                self.cursor = i;
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
            self.cursor = next;
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
        if let Some(Stop::Group(id)) = self.cursor_stop() {
            if !self.expanded.contains(&id) {
                self.expanded.insert(id);
                return; // the rows appear beneath; the cursor stays on the line
            }
        }
        let pos = self.cursor_pos();
        if pos + 1 < self.stops().len() {
            self.cursor = pos + 1;
        }
    }

    /// `←`: close — shut an open group, else out to the parent, else back one
    /// stop. The mirror of [`App::descend`]: it only ever moves earlier in the
    /// body, and the last clause is what stops it dying at depth 0.
    fn ascend(&mut self) {
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
                self.cursor = parent;
                return;
            }
        }
        if pos > 0 {
            self.cursor = pos - 1;
        }
    }

    /// Swap in freshly fetched clusters, keeping query/scope/expansions intact
    /// and the cursor pinned to stop identity (falling back to the same
    /// position, clamped, if the stop vanished — see `refresh::preserve_cursor`).
    pub fn replace_clusters(&mut self, clusters: BTreeMap<MapId, Map>) {
        let anchor = self.cursor_key();
        let old_index = self.cursor_pos();
        self.clusters = clusters;
        let new_order: Vec<StopKey> = self
            .stops()
            .iter()
            .map(|at| self.stop_key(&at.stop))
            .collect();
        self.cursor = crate::refresh::preserve_cursor(anchor.as_ref(), old_index, &new_order);
    }

    /// The first enter (#62): stage a launch of the cursor's ticket by opening
    /// the launch line — or refuse, with a count-line notice, the two things
    /// that cannot launch. Blocked is refused on *status* (its stage is
    /// unactionable, whatever it is); done is refused by [`launch::route`]
    /// returning no route — stage, not ticket state, so a merged PR on a
    /// still-open ticket refuses too.
    fn request_launch(&mut self) -> Outcome {
        // On a group line there is no agent to run, and exactly one thing the
        // key could plausibly mean — so `enter` opens or shuts it rather than
        // reporting that nothing is selected when something plainly is.
        if let Some(Stop::Group(id)) = self.cursor_stop() {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
            return Outcome::Continue;
        }
        let Some(row) = self.cursor_row() else {
            self.notice = Some("nothing selected".to_string());
            return Outcome::Continue;
        };
        let ticket = self.ticket(&row);
        if let Status::Blocked { needs } = &ticket.status {
            let needs: Vec<String> = needs.iter().map(|n| format!("#{n}")).collect();
            self.notice = Some(format!(
                "#{} is blocked — needs {}",
                ticket.number,
                needs.join(", ")
            ));
            return Outcome::Continue;
        }
        match launch::route(ticket.ticket_type, stage(&ticket.prs, &ticket.status)) {
            None => {
                self.notice = Some(format!("#{} is done — nothing to launch", ticket.number));
                Outcome::Continue
            }
            Some(route) => {
                self.overlay = Overlay::LaunchLine {
                    staged: Staged::new(ticket, row.map.number, route),
                    text: String::new(),
                };
                Outcome::Continue
            }
        }
    }

    /// The second enter: resolve the staged launch against the projects cache
    /// — straight to the loop when there is one candidate checkout, through
    /// the picker when there are several, and a notice when there is none to
    /// launch into. Which map the ticket belongs to is the cluster it sits in
    /// — a row without a map is unrepresentable, so the old "repo has no map"
    /// failure is gone with it.
    ///
    /// Everything this needs came with the [`Staged`] launch, so a refetch
    /// between the two enters cannot redirect it at another ticket.
    fn resolve_launch(&mut self, staged: &Staged, mode: &LaunchMode) -> Outcome {
        match launch::plan(&self.checkouts, staged, mode) {
            Targets::Unregistered => {
                self.notice = Some(format!(
                    "no registered checkout of {} on this machine — run wf inside one",
                    staged.repo
                ));
                Outcome::Continue
            }
            Targets::One(launch) => {
                self.notice = Some(format!("→ {}", launch.describe()));
                Outcome::Launch(launch)
            }
            Targets::Many(launches) => {
                self.notice = Some(format!(
                    "{}#{}: which checkout?",
                    staged.repo, staged.ticket
                ));
                self.overlay = Overlay::PickCheckout {
                    launches,
                    cursor: 0,
                };
                Outcome::Continue
            }
        }
    }

    /// Keys while the launch line is up (#62). The line owns every printable
    /// key — filtering already happened, this text is the launch's mode — and
    /// esc backs out to the list with the query and cursor untouched (they
    /// were never touched to begin with; the invariant is that nothing here
    /// may touch them).
    fn handle_launch_line_key(
        &mut self,
        key: KeyEvent,
        staged: Staged,
        mut text: String,
    ) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Back to the list; the overlay is already None.
            KeyCode::Esc => Outcome::Continue,
            KeyCode::Char('c') if ctrl => Outcome::Quit,
            KeyCode::Enter => self.resolve_launch(&staged, &LaunchMode::parse(&text)),
            KeyCode::Backspace => {
                text.pop();
                self.overlay = Overlay::LaunchLine { staged, text };
                Outcome::Continue
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                text.push(c);
                self.overlay = Overlay::LaunchLine { staged, text };
                Outcome::Continue
            }
            _ => {
                self.overlay = Overlay::LaunchLine { staged, text };
                Outcome::Continue
            }
        }
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
                Outcome::Launch(launch)
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
            Overlay::LaunchLine { staged, text } => {
                return self.handle_launch_line_key(key, staged, text);
            }
            Overlay::None => {}
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Outcome::Quit,
            KeyCode::Char('r') if ctrl => {
                self.notice = Some("refreshing…".to_string());
                Outcome::Refresh
            }
            KeyCode::Char('f') if ctrl => {
                if let Some(map) = self.cursor_map() {
                    let anchor = self.cursor_key();
                    self.scope = Scope::Project(map.repo);
                    if let Some(key) = anchor {
                        self.point_at(&key);
                    }
                }
                Outcome::Continue
            }
            KeyCode::Char('g') if ctrl => {
                let anchor = self.cursor_key();
                self.scope = Scope::All;
                if let Some(key) = anchor {
                    self.point_at(&key);
                }
                Outcome::Continue
            }
            // Toggle the structural lens (#51): leverage ⇄ forest. The cursor
            // stays on its ticket if the other screen shows it; a live query
            // keeps flattening either lens until it is cleared.
            KeyCode::Tab => {
                let anchor = self.cursor_key();
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
                    self.cursor = 0;
                    Outcome::Continue
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                Outcome::Continue
            }
            // `q` quits only on an empty query; mid-query it types.
            KeyCode::Char('q') if !ctrl && self.query.is_empty() => Outcome::Quit,
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.cursor = 0;
                Outcome::Continue
            }
            _ => Outcome::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Route;
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

    /// Two clusters: the wayfinder map and a dotfiles map.
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
        App::new(clusters)
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
            cluster("blooop/finished", 1, Some("2026-08-06T12:00:00Z"), false),
            cluster("blooop/stale", 2, Some("2026-08-01T09:00:00Z"), true),
            cluster("blooop/fresh", 3, Some("2026-08-05T09:00:00Z"), true),
            cluster("blooop/undated", 4, None, true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            cluster_order(&App::new(clusters)),
            vec![
                MapId::new("blooop/fresh", 3),
                MapId::new("blooop/stale", 2),
                // An unparsed stamp is not guessed into the middle of the live
                // maps: it sorts after every dated one…
                MapId::new("blooop/undated", 4),
                // …and the finished map is still below it.
                MapId::new("blooop/finished", 1),
            ]
        );
    }

    #[test]
    fn equal_activity_falls_back_to_repo_then_number() {
        // Same instant on three maps: the order has to be *some* fixed one, or
        // the screen reshuffles itself between frames for no reason.
        let stamp = Some("2026-08-06T12:00:00Z");
        let clusters: BTreeMap<MapId, Map> = [
            cluster("blooop/wayfinder", 47, stamp, true),
            cluster("kinisi/zeta", 4, stamp, true),
            cluster("blooop/wayfinder", 1, stamp, true),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            cluster_order(&App::new(clusters)),
            vec![
                MapId::new("blooop/wayfinder", 1),
                MapId::new("blooop/wayfinder", 47),
                MapId::new("kinisi/zeta", 4),
            ]
        );
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
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
        assert_eq!(app.cursor_pos(), 0);
    }

    /// What the cursor is on, as a short label: a ticket by number, a group by
    /// kind. Reads like the screen does.
    fn at(app: &App) -> String {
        match app.cursor_stop() {
            Some(Stop::Ticket(row)) => format!("#{}", app.ticket(&row).number),
            Some(Stop::Group(g)) => format!("{:?}", g.kind),
            None => "nothing".to_string(),
        }
    }

    #[test]
    fn down_walks_the_takeable_tickets_and_steps_over_their_context() {
        // Clusters order by (repo, number): dotfiles#4 before wayfinder#1.
        // The depth-0 axis is dotfiles #103, then wayfinder's takeable #6 and
        // #9, then its Done group — #7 hangs under #6 as context and is *not*
        // on this axis, which is the whole point of #57.
        let mut app = fixture_app();
        assert_eq!(at(&app), "#103");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "#6");
        app.handle_key(ctrl('j'));
        assert_eq!(at(&app), "#9", "#7 is context under #6, not a stop here");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(at(&app), "Done");
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(at(&app), "Done", "clamped at the last stop");
        app.handle_key(ctrl('k'));
        assert_eq!(at(&app), "#9");
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Up));
        }
        assert_eq!(
            at(&app),
            "#103",
            "clamped at the first stop, across clusters"
        );
    }

    #[test]
    fn right_descends_into_the_subtree_and_left_comes_back() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Down)); // wayfinder #6
        assert_eq!(at(&app), "#6");
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
        // only way left: one stop back.
        app.handle_key(key(KeyCode::Left));
        assert_eq!(at(&app), "#103");
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
        let mut app = App::new(clusters);
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
        App::new(clusters)
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
                    let mut app = knotty_app();
                    if open_group {
                        while !matches!(app.cursor_stop(), Some(Stop::Group(_))) {
                            app.handle_key(key(KeyCode::Down));
                        }
                        app.handle_key(key(KeyCode::Right));
                        app.cursor = 0;
                    }
                    let total = app.stops().len();
                    for start in 0..total {
                        app.cursor = start;
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
            app.cursor = if forward { 0 } else { total - 1 };
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
        let mut app = App::new(clusters);
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
        let mut seen = vec![at(&app)];
        for _ in 1..total {
            app.handle_key(key(KeyCode::Down));
            seen.push(at(&app));
        }
        assert_eq!(
            seen,
            vec!["#1069", "#1070", "#1071", "#1072"],
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
        app.handle_key(key(KeyCode::Down)); // wayfinder #6
        app.handle_key(key(KeyCode::Right)); // into its subtree: #7
        assert_eq!(at(&app), "#7");
        assert_eq!(app.screen(), Screen::Structured(Lens::Leverage));

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.screen(), Screen::Structured(Lens::Forest));
        // The forest shows done #2 as a row and reorders — the cursor follows
        // its ticket, not its old position.
        assert_eq!(at(&app), "#7");
        assert_eq!(app.visible().len(), 5, "the forest is total");
        assert_eq!(app.stops().len(), 5, "and holds nothing back to open");

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.screen(), Screen::Structured(Lens::Leverage));
        assert_eq!(at(&app), "#7");
    }

    #[test]
    fn a_live_query_flattens_and_clearing_it_restores_the_lens() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Tab)); // forest
        type_str(&mut app, "bread");
        assert_eq!(app.screen(), Screen::Flattened { query: "bread" });
        assert_eq!(app.visible().len(), 1);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(
            app.screen(),
            Screen::Structured(Lens::Forest),
            "esc clears the query back to the lens it flattened"
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
        let checkout = |path: &str, repo: &str| Checkout {
            path: std::path::PathBuf::from(path),
            repo: repo.to_string(),
        };
        fixture_app().with_checkouts(vec![
            checkout("/data/proj/wayfinder", "blooop/wayfinder"),
            checkout("/data/k1/dotfiles", "blooop/dotfiles"),
            checkout("/data/k2/dotfiles", "blooop/dotfiles"),
        ])
    }

    #[test]
    fn enter_opens_the_launch_line_and_a_second_enter_launches() {
        // The two-step (#62): the first enter stages the launch — nothing
        // execs yet — and an empty line's enter is the interactive default.
        let mut app = launchable_app();
        // Move to wayfinder#6, whose repo has one checkout.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::LaunchLine { staged, text } => {
                assert_eq!(staged.route, Route::Wayfinder, "a task is a decision node");
                assert_eq!(staged.ticket, 6);
                assert_eq!(staged.title, "Re-entry breadcrumbs", "the line names it");
                assert_eq!(text, "", "the line opens empty");
            }
            other => panic!("expected the launch line, got {other:?}"),
        }
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.key(), "wayfinder#6");
        assert_eq!(launch.cwd(), std::path::Path::new("/data/proj/wayfinder"));
        // The map issue is the cluster's — not a per-repo lookup.
        assert_eq!(launch.agent_argv().last().unwrap(), "/wayfinder 1 6");
        assert_eq!(app.overlay, Overlay::None, "one candidate must not prompt");
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("wayfinder#6 in /data/proj/wayfinder"));
    }

    #[test]
    fn the_same_ticket_on_two_maps_launches_with_the_cluster_it_was_picked_in() {
        // One repo, two open maps, both listing ticket #6: the row's cluster —
        // not the repo — decides `/wayfinder`'s map argument.
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
        let mut app = App::new(clusters).with_checkouts(vec![Checkout {
            path: std::path::PathBuf::from("/data/proj/wayfinder"),
            repo: "blooop/wayfinder".to_string(),
        }]);
        // Row 0 is map #1's copy, row 1 is map #47's.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter)); // stage it
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/wayfinder 47 6");
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
        let mut app = launchable_app();
        // Cursor starts on dotfiles#103 — two checkouts. The first enter
        // stages the launch; the second resolves it to the picker.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickCheckout { launches, cursor } => {
                assert_eq!(launches.len(), 2);
                assert_eq!(*cursor, 0);
            }
            other @ (Overlay::None | Overlay::LaunchLine { .. }) => {
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
        let mut app = launchable_app();
        app.handle_key(key(KeyCode::Enter)); // dotfiles#103: stage the launch
        app.handle_key(key(KeyCode::Enter)); // resolve — two checkouts: picker
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Down));
        }
        match &app.overlay {
            Overlay::PickCheckout { cursor, .. } => assert_eq!(*cursor, 1),
            other @ (Overlay::None | Overlay::LaunchLine { .. }) => {
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
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let notice = app.notice.as_deref().unwrap();
        assert!(
            notice.contains("no registered checkout"),
            "notice: {notice}"
        );
    }

    #[test]
    fn launch_line_typing_lands_in_the_line_and_esc_restores_the_list() {
        // The line owns every printable key: nothing leaks into the query.
        // Esc backs out with the query and the cursor exactly as they were.
        let mut app = launchable_app();
        type_str(&mut app, "bread"); // flattens to wayfinder#6
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(key(KeyCode::Enter));
        type_str(&mut app, "half a thought");
        app.handle_key(key(KeyCode::Backspace));
        match &app.overlay {
            Overlay::LaunchLine { text, .. } => assert_eq!(text, "half a though"),
            other => panic!("expected the launch line, got {other:?}"),
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
        // And esc means *back to the list*, never quit-from-the-line: the next
        // esc clears the query, the one after quits — the ordinary ladder.
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Continue);
        assert!(app.query.is_empty());
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Outcome::Quit);
    }

    #[test]
    fn defer_text_on_the_launch_line_launches_deferred_with_steering() {
        // The acceptance shape: enter → `defer something` → enter produces a
        // command carrying `defer: something`.
        let mut app = launchable_app();
        app.handle_key(key(KeyCode::Down)); // wayfinder#6, one checkout
        app.handle_key(key(KeyCode::Enter));
        type_str(&mut app, "defer something");
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch.agent_argv().last().unwrap(),
            "/wayfinder 1 6 defer: something"
        );
    }

    #[test]
    fn the_typed_mode_survives_the_checkout_picker() {
        // Two checkouts of dotfiles: the mode is settled on the line, the
        // picker only answers *where* — the pick must not lose the defer.
        let mut app = launchable_app();
        app.handle_key(key(KeyCode::Enter)); // dotfiles#103: stage
        type_str(&mut app, "defer");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert!(matches!(app.overlay, Overlay::PickCheckout { .. }));
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(
            launch.agent_argv().last().unwrap(),
            "/wayfinder 4 103 defer"
        );
    }

    #[test]
    fn a_staged_launch_survives_a_refetch_moving_its_ticket() {
        // The line stays up while background fetches are still landing (#27),
        // and each arrival swaps the clusters underneath it. A staged launch
        // is snapshotted index-free, so the line keeps naming — and launching
        // — the ticket it was opened on, wherever that ticket now sits.
        let staged = || {
            let mut app = launchable_app();
            app.handle_key(key(KeyCode::Down)); // wayfinder#6, at index 1
            app.handle_key(key(KeyCode::Enter));
            type_str(&mut app, "defer");
            app
        };
        let wf = MapId::new("blooop/wayfinder", 1);

        // Reordered: index 1 now names #9, not #6.
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
            launch.agent_argv().last().unwrap(),
            "/wayfinder 1 6 defer",
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
            Overlay::LaunchLine { staged, .. } => assert_eq!(staged.ticket, 6),
            other => panic!("expected the launch line, got {other:?}"),
        }
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/wayfinder 1 6 defer");
    }

    #[test]
    fn enter_on_a_done_or_blocked_node_is_a_notice_not_a_launch_line() {
        let mut app = launchable_app();
        // Blocked: #7 hangs under #6 as context.
        app.handle_key(key(KeyCode::Down)); // #6
        app.handle_key(key(KeyCode::Right)); // into #7
        assert_eq!(at(&app), "#7");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert_eq!(app.overlay, Overlay::None, "no line on a blocked node");
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
        assert_eq!(app.overlay, Overlay::None, "no line on a done node");
        assert!(
            app.notice.as_deref().unwrap().contains("done"),
            "{:?}",
            app.notice
        );
    }

    #[test]
    fn a_build_node_routes_by_its_stage() {
        // One build ticket, staged by its PR: in review → /review; the same
        // ticket with no PR evidence is ready → /tdd.
        let build_app = |prs: Vec<PrLink>| -> App {
            let mut t = ticket(
                "blooop/wayfinder",
                65,
                "Author the /tdd skill",
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
            App::new(clusters).with_checkouts(vec![Checkout {
                path: std::path::PathBuf::from("/data/proj/wayfinder"),
                repo: "blooop/wayfinder".to_string(),
            }])
        };

        let mut ready = build_app(vec![]);
        ready.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(
                &ready.overlay,
                Overlay::LaunchLine {
                    staged: Staged {
                        route: Route::Tdd,
                        ..
                    },
                    ..
                }
            ),
            "{:?}",
            ready.overlay
        );
        let launch = match ready.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/tdd 65");

        let mut in_review = build_app(vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }]);
        in_review.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(
                &in_review.overlay,
                Overlay::LaunchLine {
                    staged: Staged {
                        route: Route::Review,
                        ..
                    },
                    ..
                }
            ),
            "{:?}",
            in_review.overlay
        );
        let launch = match in_review.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.agent_argv().last().unwrap(), "/review 65");

        // A merged PR with nothing open means done — stage, not ticket state,
        // is what refuses the launch.
        let mut done = build_app(vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Merged,
        }]);
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
    fn ctrl_f_focuses_cursor_rows_project_and_ctrl_g_widens() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Down)); // cursor on wayfinder #6
        app.handle_key(ctrl('f'));
        assert_eq!(app.scope, Scope::Project("blooop/wayfinder".to_string()));
        assert_eq!(
            app.visible().len(),
            3,
            "leverage rows: #6, #7 beneath it, #9"
        );
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(ctrl('g'));
        assert_eq!(app.scope, Scope::All);
        assert_eq!(app.visible().len(), 4);
        // cursor stayed anchored on the same ticket
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
    }

    #[test]
    fn focus_keeps_every_open_map_of_the_repo() {
        // Two maps on one repo: focusing the repo is not focusing one map.
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
        let mut app = App::new(clusters);
        app.handle_key(key(KeyCode::Down)); // cursor onto wayfinder#6 (map #1)
        app.handle_key(ctrl('f'));
        assert_eq!(app.scope, Scope::Project("blooop/wayfinder".to_string()));
        let visible = app.visible();
        assert_eq!(visible.len(), 2, "both wayfinder maps stay on screen");
        assert!(visible.iter().all(|row| row.map.repo == "blooop/wayfinder"));
    }

    #[test]
    fn ctrl_r_requests_refresh_and_replace_clusters_keeps_anchor() {
        let mut app = fixture_app();
        assert_eq!(app.handle_key(ctrl('r')), Outcome::Refresh);
        app.handle_key(key(KeyCode::Down)); // cursor on wayfinder#6
        let same = app.clusters.clone();
        app.replace_clusters(same);
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
    }

    #[test]
    fn replace_clusters_does_not_teleport_when_cursor_ticket_vanishes() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Down)); // cursor on wayfinder#6, position 1
        let mut smaller = app.clusters.clone();
        smaller
            .get_mut(&MapId::new("blooop/wayfinder", 1))
            .unwrap()
            .tickets
            .retain(|t| t.number != 6);
        app.replace_clusters(smaller);
        // Identity gone: cursor stays at the same position, clamped.
        assert_eq!(app.cursor_pos(), 1);
        assert_eq!(app.cursor_ticket().unwrap().number, 9);
    }

    #[test]
    fn focus_separates_a_fork_from_its_upstream() {
        // Two repos sharing a short name: identity and scope are the slug,
        // so focusing one must not drag the other's rows in.
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
        let mut app = App::new(clusters);
        app.handle_key(key(KeyCode::Down)); // cursor on upstream/dotfiles#5
        assert_eq!(app.cursor_ticket().unwrap().repo, "upstream/dotfiles");
        app.handle_key(ctrl('f'));
        assert_eq!(app.scope, Scope::Project("upstream/dotfiles".to_string()));
        assert_eq!(
            app.visible().len(),
            1,
            "the fork's identically-numbered row must not show"
        );
        assert_eq!(app.cursor_ticket().unwrap().repo, "upstream/dotfiles");
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
