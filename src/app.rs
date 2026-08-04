//! Main-screen state and the Build 2 keybinding skeleton.
//!
//! [`App`] owns everything the screen needs between keypresses: the map, the
//! fuzzy query, the cursor over *visible* rows, the project scope, and a
//! one-shot notice line. Key handling returns an [`Outcome`] so the event
//! loop owns side effects (quit, refetch) — the `Refresh` outcome is the
//! same seam a background poller can drive later (#17).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::filter::matching_indices;
use crate::model::{Map, Ticket, GROUP_LABELS};

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
}

pub struct App {
    pub map: Map,
    pub query: String,
    pub scope: Scope,
    /// One-shot status message shown on the count line; cleared on the next
    /// keypress. Carries the visible no-op for `enter` until Build 4.
    pub notice: Option<String>,
    cursor: usize,
}

impl App {
    pub fn new(map: Map) -> Self {
        Self {
            map,
            query: String::new(),
            scope: Scope::All,
            notice: None,
            cursor: 0,
        }
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

    /// Handle one keypress. Typing edits the query (rows re-filter, cursor
    /// jumps to the first visible row); see the ticket #14 skeleton for the
    /// chord bindings.
    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        self.notice = None;
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
            KeyCode::Enter => {
                // Visible no-op until the launch seam lands (Build 4).
                self.notice = Some(match self.cursor_ticket() {
                    Some(t) => format!("enter → launch {}#{} — wired in Build 4", t.repo, t.number),
                    None => "nothing selected".to_string(),
                });
                Outcome::Continue
            }
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
    use crate::model::{classify, Map, Ticket};

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

    #[test]
    fn enter_is_a_visible_noop_stub() {
        let mut app = fixture_app();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Outcome::Continue);
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("wayfinder#6"), "notice was: {notice}");
        assert!(notice.contains("Build 4"), "notice was: {notice}");
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
