//! Main-screen state and the keybindings (#14).
//!
//! [`App`] owns everything the screen needs between keypresses: the clusters
//! (one per open map, #50), the fuzzy query, the cursor over *visible* rows,
//! the project scope, and a one-shot notice line. Key handling returns an
//! [`Outcome`] so the binary owns the side effects: the app decides *what* to
//! launch and `main` is the only thing that may act on it, because acting on
//! it means giving the terminal back and never coming here again (#34).

use std::collections::{BTreeMap, BTreeSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::filter::matching_indices;
use crate::launch::{self, Launch, Targets};
use crate::model::{Map, MapId, MapSet, Ticket, GROUP_LABELS};
use crate::projects::Checkout;
use crate::refresh::Startup;

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

/// A modal layer over the main screen. Only one thing needs one: a repo with
/// several registered checkouts (the k1–k5 pattern) must be asked which tree
/// the agent runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// Candidates are complete launches, so the pick cannot produce an
    /// inconsistent one.
    PickCheckout { launches: Vec<Launch>, cursor: usize },
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

pub struct App {
    /// The clusters on screen: every open map that has arrived, in
    /// (repo, number) order — which is also render order.
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

    /// The clusters currently in scope, in render order.
    pub fn scoped_clusters(&self) -> Vec<(&MapId, &Map)> {
        self.clusters
            .iter()
            .filter(|(id, _)| self.in_scope(id))
            .collect()
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

    /// Rows visible right now, in on-screen order: cluster-major (maps in
    /// (repo, number) order), group-major within a cluster (frontier / claimed
    /// / blocked / done), map order within a group. The cursor indexes this
    /// list — cluster and group headers are never cursor stops.
    pub fn visible(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for (id, map) in self.scoped_clusters() {
            let matched = matching_indices(&map.tickets, &self.query);
            for group in 0..GROUP_LABELS.len() {
                out.extend(
                    matched
                        .iter()
                        .copied()
                        .filter(|&i| map.tickets[i].status.group() == group)
                        .map(|index| Row {
                            map: id.clone(),
                            index,
                        }),
                );
            }
        }
        out
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

    /// Cursor position clamped into the visible list.
    pub fn cursor_pos(&self) -> usize {
        self.cursor.min(self.visible().len().saturating_sub(1))
    }

    /// The row under the cursor, if any is visible.
    pub fn cursor_row(&self) -> Option<Row> {
        self.visible().get(self.cursor_pos()).cloned()
    }

    /// The ticket under the cursor, if any row is visible.
    pub fn cursor_ticket(&self) -> Option<&Ticket> {
        self.cursor_row().map(|row| {
            let map: &Map = &self.clusters[&row.map];
            &map.tickets[row.index]
        })
    }

    /// The cursor row's stable identity.
    fn cursor_key(&self) -> Option<RowKey> {
        self.cursor_row().map(|row| self.row_key(&row))
    }

    /// Point the cursor at a specific row if it is visible.
    fn point_at(&mut self, key: &RowKey) {
        let pos = self
            .visible()
            .iter()
            .position(|row| &self.row_key(row) == key);
        self.cursor = pos.unwrap_or(0);
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let pos = self.cursor_pos() as isize + delta;
        self.cursor = pos.clamp(0, len as isize - 1) as usize;
    }

    /// Swap in freshly fetched clusters, keeping query/scope intact and the
    /// cursor pinned to row identity (falling back to the same position,
    /// clamped, if the row vanished — see `refresh::preserve_cursor`).
    pub fn replace_clusters(&mut self, clusters: BTreeMap<MapId, Map>) {
        let anchor = self.cursor_key();
        let old_index = self.cursor_pos();
        self.clusters = clusters;
        let new_order: Vec<RowKey> = self
            .visible()
            .iter()
            .map(|row| self.row_key(row))
            .collect();
        self.cursor = crate::refresh::preserve_cursor(anchor.as_ref(), old_index, &new_order);
    }

    /// Resolve a launch of the cursor's ticket: straight to the loop when
    /// there is one candidate checkout, through the picker when there are
    /// several, and a notice when there is none to launch into. Which map the
    /// ticket belongs to is the cluster it sits in — a row without a map is
    /// unrepresentable, so the old "repo has no map" failure is gone with it.
    fn request_launch(&mut self) -> Outcome {
        let Some(row) = self.cursor_row() else {
            self.notice = Some("nothing selected".to_string());
            return Outcome::Continue;
        };
        let ticket = self.ticket(&row).clone();
        let map_issue = row.map.number;
        match launch::plan(&self.checkouts, &ticket, map_issue) {
            Targets::Unregistered => {
                self.notice = Some(format!(
                    "no registered checkout of {} on this machine — run wf inside one",
                    ticket.repo
                ));
                Outcome::Continue
            }
            Targets::One(launch) => {
                self.notice = Some(format!("→ {}", launch.describe()));
                Outcome::Launch(launch)
            }
            Targets::Many(launches) => {
                self.notice = Some(format!("{}#{}: which checkout?", ticket.repo, ticket.number));
                self.overlay = Overlay::PickCheckout { launches, cursor: 0 };
                Outcome::Continue
            }
        }
    }

    /// Keys while the checkout picker is up. The modal owns every key: no
    /// typing leaks into the query behind it.
    fn handle_overlay_key(&mut self, key: KeyEvent, launches: Vec<Launch>, cursor: usize) -> Outcome {
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
        if let Overlay::PickCheckout { launches, cursor } =
            std::mem::replace(&mut self.overlay, Overlay::None)
        {
            return self.handle_overlay_key(key, launches, cursor);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Outcome::Quit,
            KeyCode::Char('r') if ctrl => {
                self.notice = Some("refreshing…".to_string());
                Outcome::Refresh
            }
            KeyCode::Char('f') if ctrl => {
                if let Some(key) = self.cursor_key() {
                    self.scope = Scope::Project(key.map.repo.clone());
                    self.point_at(&key);
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
            KeyCode::Down => {
                self.move_cursor(1);
                Outcome::Continue
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                Outcome::Continue
            }
            KeyCode::Char('j') if ctrl => {
                self.move_cursor(1);
                Outcome::Continue
            }
            KeyCode::Char('k') if ctrl => {
                self.move_cursor(-1);
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
    use crate::model::{classify, TicketType};

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
        }
    }

    /// Two clusters: the wayfinder map and a dotfiles map.
    fn fixture_app() -> App {
        let mut clusters = BTreeMap::new();
        clusters.insert(
            MapId::new("blooop/wayfinder", 1),
            Map {
                title: "Map: wf".to_string(),
                tickets: vec![
                    ticket("blooop/wayfinder", 2, "Choose the stack", false, true, vec![]),
                    ticket("blooop/wayfinder", 6, "Re-entry breadcrumbs", true, false, vec![]),
                    ticket("blooop/wayfinder", 7, "Supervising AFK agents", true, false, vec![6]),
                    ticket("blooop/wayfinder", 9, "Main screen design", true, true, vec![]),
                ],
            },
        );
        clusters.insert(
            MapId::new("blooop/dotfiles", 4),
            Map {
                title: "Map: dotfiles".to_string(),
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

    #[test]
    fn cursor_moves_cluster_major_then_group_major_and_clamps() {
        let mut app = fixture_app();
        // Clusters order by (repo, number): dotfiles#4 before wayfinder#1.
        // On-screen: dotfiles frontier #103 · wayfinder frontier #6, claimed #9,
        // blocked #7, done #2.
        assert_eq!(app.cursor_ticket().unwrap().number, 103);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(ctrl('j'));
        assert_eq!(app.cursor_ticket().unwrap().number, 9);
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(app.cursor_ticket().unwrap().number, 2); // clamped at last row
        app.handle_key(ctrl('k'));
        assert_eq!(app.cursor_ticket().unwrap().number, 7);
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Up));
        }
        assert_eq!(app.cursor_ticket().unwrap().number, 103); // clamped at first row
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
    fn enter_launches_the_cursors_ticket_with_its_clusters_map() {
        let mut app = launchable_app();
        // Move to wayfinder#6, whose repo has one checkout.
        app.handle_key(key(KeyCode::Down));
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
                    tickets: vec![ticket("blooop/wayfinder", 6, "Shared ticket", true, false, vec![])],
                },
            );
        }
        let mut app = App::new(clusters).with_checkouts(vec![Checkout {
            path: std::path::PathBuf::from("/data/proj/wayfinder"),
            repo: "blooop/wayfinder".to_string(),
        }]);
        // Row 0 is map #1's copy, row 1 is map #47's.
        app.handle_key(key(KeyCode::Down));
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
        // Cursor starts on dotfiles#103 — two checkouts.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        match &app.overlay {
            Overlay::PickCheckout { launches, cursor } => {
                assert_eq!(launches.len(), 2);
                assert_eq!(*cursor, 0);
            }
            other => panic!("expected the picker, got {other:?}"),
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
        app.handle_key(key(KeyCode::Enter)); // dotfiles#103 under the cursor
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Down));
        }
        match &app.overlay {
            Overlay::PickCheckout { cursor, .. } => assert_eq!(*cursor, 1),
            other => panic!("expected the picker, got {other:?}"),
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
        // cluster and cannot be missing.
        let mut app = fixture_app();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("no registered checkout"), "notice: {notice}");
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
        assert_eq!(app.visible().len(), 4);
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(ctrl('g'));
        assert_eq!(app.scope, Scope::All);
        assert_eq!(app.visible().len(), 5);
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
                tickets: vec![ticket("blooop/wayfinder", 6, "t6", true, false, vec![])],
            },
        );
        clusters.insert(
            MapId::new("blooop/wayfinder", 47),
            Map {
                title: "Map: selection view".to_string(),
                tickets: vec![ticket("blooop/wayfinder", 50, "t50", true, false, vec![])],
            },
        );
        clusters.insert(
            MapId::new("blooop/dotfiles", 4),
            Map {
                title: "Map: dotfiles".to_string(),
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
        assert_eq!(app.visible().len(), 1, "the fork's identically-numbered row must not show");
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
