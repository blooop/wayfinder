//! Main-screen state and the keybindings (#14).
//!
//! [`App`] owns everything the screen needs between keypresses: the map, the
//! fuzzy query, the cursor over *visible* rows, the project scope, and a
//! one-shot notice line. Key handling returns an [`Outcome`] so the binary owns
//! the side effects: the app decides *what* to launch and `main` is the only
//! thing that may act on it, because acting on it means giving the terminal
//! back and never coming here again (#34).

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::filter::matching_indices;
use crate::launch::{self, Launch, MapIssues, Targets};
use crate::model::{merge_maps, Map, Ticket, GROUP_LABELS};
use crate::projects::Checkout;
use crate::refresh::Startup;

/// Project scope: everything, or one repo focused via `ctrl-f`.
/// With a single repo synced this is a near-no-op, but the state is wired
/// so multi-project (Build 3) only has to feed more tickets in.
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
    /// Force a refetch (`ctrl-r`). The loop performs it and puts the result
    /// back via [`App::replace_map`].
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

pub struct App {
    pub map: Map,
    pub query: String,
    pub scope: Scope,
    /// One-shot status message shown on the count line; cleared on the next
    /// keypress.
    pub notice: Option<String>,
    /// Launch input from the projects cache (#15 handoff): which checkouts
    /// exist on this machine.
    pub checkouts: Vec<Checkout>,
    /// Each repo's map issue number — `/wayfinder`'s first argument.
    pub map_issues: MapIssues,
    /// How much of the initial load has landed (#27). The screen is drawn
    /// before any of it, so this is what stops an empty list from reading as
    /// "no tickets" while the fetch is still out.
    pub startup: Startup,
    pub overlay: Overlay,
    cursor: usize,
}

impl App {
    /// An app over a map already in hand — so nothing is being waited on.
    pub fn new(map: Map) -> Self {
        Self {
            map,
            query: String::new(),
            scope: Scope::All,
            notice: None,
            checkouts: Vec::new(),
            map_issues: MapIssues::new(),
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
            ..Self::new(merge_maps(&BTreeMap::new()))
        }
    }

    /// Attach the launch inputs: the cached checkouts (the candidate trees an
    /// agent could run in) and the per-repo map issue numbers.
    pub fn with_projects(mut self, checkouts: Vec<Checkout>, map_issues: MapIssues) -> Self {
        self.checkouts = checkouts;
        self.map_issues = map_issues;
        self
    }

    fn in_scope(&self, ticket: &Ticket) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Project(repo) => &ticket.repo == repo,
        }
    }

    /// Ticket indices currently in scope (before the fuzzy query).
    pub fn scoped(&self) -> Vec<usize> {
        (0..self.map.tickets.len())
            .filter(|&i| self.in_scope(&self.map.tickets[i]))
            .collect()
    }

    /// Ticket indices visible right now, in on-screen order: group-major
    /// (frontier / claimed / blocked / done), map order within a group.
    /// The cursor indexes this list — group headers are never cursor stops.
    pub fn visible(&self) -> Vec<usize> {
        let matched = matching_indices(&self.map.tickets, &self.query);
        let mut out = Vec::new();
        for group in 0..GROUP_LABELS.len() {
            out.extend(matched.iter().copied().filter(|&i| {
                let t = &self.map.tickets[i];
                t.status.group() == group && self.in_scope(t)
            }));
        }
        out
    }

    /// Cursor position clamped into the visible list.
    pub fn cursor_pos(&self) -> usize {
        self.cursor.min(self.visible().len().saturating_sub(1))
    }

    /// The ticket under the cursor, if any row is visible.
    pub fn cursor_ticket(&self) -> Option<&Ticket> {
        self.visible()
            .get(self.cursor_pos())
            .map(|&i| &self.map.tickets[i])
    }

    /// Point the cursor at a specific ticket if it is visible.
    fn point_at(&mut self, repo: &str, number: u64) {
        if let Some(pos) = self.visible().iter().position(|&i| {
            let t = &self.map.tickets[i];
            t.repo == repo && t.number == number
        }) {
            self.cursor = pos;
        } else {
            self.cursor = 0;
        }
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

    /// Swap in freshly fetched data, keeping query/scope intact and the
    /// cursor pinned to ticket identity (falling back to the same position,
    /// clamped, if the ticket vanished — see `refresh::preserve_cursor`).
    pub fn replace_map(&mut self, map: Map) {
        let anchor = self.cursor_ticket().map(|t| (t.repo.clone(), t.number));
        let old_index = self.cursor_pos();
        self.map = map;
        let visible = self.visible();
        let new_order: Vec<(&str, u64)> = visible
            .iter()
            .map(|&i| {
                let t = &self.map.tickets[i];
                (t.repo.as_str(), t.number)
            })
            .collect();
        self.cursor = crate::refresh::preserve_cursor(
            anchor.as_ref().map(|(repo, number)| (repo.as_str(), *number)),
            old_index,
            &new_order,
        );
    }

    /// Resolve a launch of the cursor's ticket: straight to the loop when
    /// there is one candidate checkout, through the picker when there are
    /// several, and a notice when there is none to launch into.
    fn request_launch(&mut self) -> Outcome {
        let Some(ticket) = self.cursor_ticket().cloned() else {
            self.notice = Some("nothing selected".to_string());
            return Outcome::Continue;
        };
        let (repo, number) = (&ticket.repo, ticket.number);
        let Some(&map_issue) = self.map_issues.get(repo) else {
            self.notice = Some(format!("{repo} has no map — nothing to hand /wayfinder"));
            return Outcome::Continue;
        };
        match launch::plan(&self.checkouts, &ticket, map_issue) {
            Targets::Unregistered => {
                self.notice = Some(format!(
                    "no registered checkout of {repo} on this machine — run wf inside one"
                ));
                Outcome::Continue
            }
            Targets::One(launch) => {
                self.notice = Some(format!("→ {}", launch.describe()));
                Outcome::Launch(launch)
            }
            Targets::Many(launches) => {
                self.notice = Some(format!("{repo}#{number}: which checkout?"));
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
                if let Some(t) = self.cursor_ticket() {
                    let (repo, number) = (t.repo.clone(), t.number);
                    self.scope = Scope::Project(repo.clone());
                    self.point_at(&repo, number);
                }
                Outcome::Continue
            }
            KeyCode::Char('g') if ctrl => {
                let anchor = self.cursor_ticket().map(|t| (t.repo.clone(), t.number));
                self.scope = Scope::All;
                if let Some((repo, number)) = anchor {
                    self.point_at(&repo, number);
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
    use crate::model::{classify, Map, Ticket, TicketType};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn fixture_app() -> App {
        let t = |repo: &str, number: u64, title: &str, open: bool, assigned: bool, needs: Vec<u64>| {
            Ticket {
                repo: repo.to_string(),
                number,
                title: title.to_string(),
                status: classify(open, assigned, needs),
                ticket_type: TicketType::Task,
            }
        };
        App::new(Map {
            repo: "blooop/wayfinder".to_string(),
            title: "Map: wf".to_string(),
            tickets: vec![
                t("blooop/wayfinder", 2, "Choose the stack", false, true, vec![]),
                t("blooop/wayfinder", 6, "Re-entry breadcrumbs", true, false, vec![]),
                t("blooop/wayfinder", 7, "Supervising AFK agents", true, false, vec![6]),
                t("blooop/wayfinder", 9, "Main screen design", true, true, vec![]),
                t("blooop/dotfiles", 103, "Prune legacy bash aliases", true, false, vec![]),
            ],
        })
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
    fn cursor_moves_over_ticket_rows_in_group_order_and_clamps() {
        let mut app = fixture_app();
        // On-screen order: frontier #6, #103 · claimed #9 · blocked #7 · done #2.
        assert_eq!(app.cursor_ticket().unwrap().number, 6);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.cursor_ticket().unwrap().number, 103);
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
        assert_eq!(app.cursor_ticket().unwrap().number, 6); // clamped at first row
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
    /// a multi-checkout repo, and map issues for both.
    fn launchable_app() -> App {
        let checkout = |path: &str, repo: &str| Checkout {
            path: std::path::PathBuf::from(path),
            repo: repo.to_string(),
        };
        let mut map_issues = MapIssues::new();
        map_issues.insert("blooop/wayfinder".to_string(), 1);
        map_issues.insert("blooop/dotfiles".to_string(), 4);
        fixture_app().with_projects(
            vec![
                checkout("/data/proj/wayfinder", "blooop/wayfinder"),
                checkout("/data/k1/dotfiles", "blooop/dotfiles"),
                checkout("/data/k2/dotfiles", "blooop/dotfiles"),
            ],
            map_issues,
        )
    }

    #[test]
    fn enter_launches_the_cursors_ticket_without_prompting() {
        let mut app = launchable_app();
        // Cursor starts on frontier wayfinder#6, whose repo has one checkout.
        let launch = match app.handle_key(key(KeyCode::Enter)) {
            Outcome::Launch(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        assert_eq!(launch.key(), "wayfinder#6");
        assert_eq!(launch.cwd(), std::path::Path::new("/data/proj/wayfinder"));
        assert_eq!(launch.agent_argv().last().unwrap(), "/wayfinder 1 6");
        assert_eq!(app.overlay, Overlay::None, "one candidate must not prompt");
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("wayfinder#6 in /data/proj/wayfinder"));
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
        app.handle_key(key(KeyCode::Down)); // dotfiles#103 — two checkouts
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
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
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
    fn a_repo_with_no_checkout_or_no_map_says_so_instead_of_launching() {
        // No launch inputs at all: every ticket is unlaunchable, and the
        // reason is the missing map (nothing to hand /wayfinder).
        let mut app = fixture_app();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        assert!(app.notice.as_deref().unwrap().contains("has no map"));

        // Map known, checkout gone from the cache: the other failure.
        let mut map_issues = MapIssues::new();
        map_issues.insert("blooop/wayfinder".to_string(), 1);
        let mut app = fixture_app().with_projects(Vec::new(), map_issues);
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
        app.handle_key(key(KeyCode::Down)); // cursor on dotfiles #103
        app.handle_key(ctrl('f'));
        assert_eq!(app.scope, Scope::Project("blooop/dotfiles".to_string()));
        assert_eq!(app.visible().len(), 1);
        assert_eq!(app.cursor_ticket().unwrap().number, 103);
        app.handle_key(ctrl('g'));
        assert_eq!(app.scope, Scope::All);
        assert_eq!(app.visible().len(), 5);
        // cursor stayed anchored on the same ticket
        assert_eq!(app.cursor_ticket().unwrap().number, 103);
    }

    #[test]
    fn ctrl_r_requests_refresh_and_replace_map_keeps_anchor() {
        let mut app = fixture_app();
        assert_eq!(app.handle_key(ctrl('r')), Outcome::Refresh);
        app.handle_key(key(KeyCode::Down)); // cursor on #103
        let same = app.map.clone();
        app.replace_map(same);
        assert_eq!(app.cursor_ticket().unwrap().number, 103);
    }

    #[test]
    fn replace_map_does_not_teleport_when_cursor_ticket_vanishes() {
        let mut app = fixture_app();
        app.handle_key(key(KeyCode::Down)); // cursor on #103, position 1
        let mut smaller = app.map.clone();
        smaller.tickets.retain(|t| t.number != 103);
        app.replace_map(smaller);
        // Identity gone: cursor stays at the same position, clamped.
        assert_eq!(app.cursor_pos(), 1);
        assert_eq!(app.cursor_ticket().unwrap().number, 9);
    }

    #[test]
    fn focus_separates_a_fork_from_its_upstream() {
        // Two repos sharing a short name: identity and scope are the slug,
        // so focusing one must not drag the other's rows in.
        let t = |repo: &str, number: u64| Ticket {
            repo: repo.to_string(),
            number,
            title: "Prune legacy bash aliases".to_string(),
            status: classify(true, false, vec![]),
            ticket_type: TicketType::Task,
        };
        let mut app = App::new(Map {
            repo: "2 projects".to_string(),
            title: "wf".to_string(),
            tickets: vec![t("blooop/dotfiles", 5), t("upstream/dotfiles", 5)],
        });
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
