//! `wf` binary — accretive multi-project startup (Build 3, #15):
//!
//! 1. If the cwd is inside a git checkout with a GitHub `origin`, register
//!    it in the per-machine cache (`~/.cache/wf/projects.json`) — explicit
//!    use *is* the registration act (the zoxide model, per #4).
//! 2. One `wayfinder:map` label search across every cached repo finds
//!    which have maps; repos without maps stay cached but hidden.
//! 3. Every map is fetched and merged into one grouped list; one poller
//!    per repo keeps it fresh (Build 5's two-tier poll, tagged by slug).
//! 4. Inside a checkout whose repo has a map, the screen opens focused on
//!    it (`Scope::Project`, repo column dropped); `ctrl-g` widens to all
//!    projects, `ctrl-f` re-focuses. Outside any checkout: all projects.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedReceiver;

use wf::app::{App, Outcome, Scope};
use wf::model::{merge_maps, Map};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{Freshness, Poller, RefreshEvent};

#[tokio::main]
async fn main() -> Result<()> {
    // Accretive registration: running wf here is what makes this checkout
    // a project. Non-checkouts and non-GitHub remotes are simply None.
    let cwd = std::env::current_dir().context("cannot resolve the working directory")?;
    let here = projects::discover_checkout(&cwd).await;
    let cache_path =
        projects::default_cache_path().context("cannot resolve the XDG cache directory")?;
    let mut cache = ProjectsCache::load_or_default(&cache_path);
    if let Some((path, slug)) = &here {
        cache.register(path.clone(), slug.clone());
        cache.save(&cache_path)?;
    }

    // Map detection: one label search intersected with the cached remotes.
    let repos = cache.repos();
    eprintln!("wf: {} cached repo(s); searching for maps…", repos.len());
    let map_issues: BTreeMap<String, u64> = wf::fetch::find_maps(&repos)
        .await?
        .into_iter()
        .collect();

    // Initial fetch of every map, merged into the one list the screen shows.
    let mut maps: BTreeMap<String, Map> = BTreeMap::new();
    let mut startup_notice = None;
    for (slug, &number) in &map_issues {
        let (owner, name) = slug.split_once('/').expect("slug is owner/name");
        eprintln!("wf: fetching {slug} map (#{number})…");
        match wf::fetch::fetch_map(owner, name, number).await {
            Ok(map) => {
                maps.insert(slug.clone(), map);
            }
            Err(err) => {
                eprintln!("wf: {slug}: {err}");
                startup_notice = Some(format!("{slug}: initial fetch failed"));
            }
        }
    }
    if maps.is_empty() && !map_issues.is_empty() {
        anyhow::bail!("every map fetch failed — check network and `gh auth status`");
    }

    let mut app = App::new(merge_maps(&maps));
    app.notice = startup_notice;
    match &here {
        // cwd-open focuses the project (lazygit-style) when its repo has a
        // map; the repo column drops because the header names the project.
        Some((_, slug)) if maps.contains_key(slug) => {
            app.scope = Scope::Project(slug.clone());
        }
        Some((_, slug)) => {
            app.notice = Some(format!("{slug} has no wayfinder:map — showing all projects"));
        }
        None => {}
    }

    // One poller per repo-with-map, all feeding one slug-tagged channel.
    let pollers: Vec<Poller> = map_issues
        .iter()
        .map(|(slug, &number)| {
            let (owner, name) = slug.split_once('/').expect("slug is owner/name");
            Poller::new(owner, name, number)
        })
        .collect();
    let updates = wf::refresh::spawn_all(pollers);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app, maps, map_issues, updates).await;
    ratatui::restore();
    result
}

/// Poll outcomes folded into displayable state: when data was last verified
/// fresh, and whether the latest poll failed. With several repos this is an
/// aggregate: any success bumps the timestamp, any failure marks stale
/// until the next success — coarse, but honest enough for a count-line hint.
struct RefreshState {
    last_verified: Option<Instant>,
    failing: bool,
}

impl RefreshState {
    fn apply(&mut self, event: &RefreshEvent) {
        match event {
            RefreshEvent::Updated(_) | RefreshEvent::Unchanged => {
                self.last_verified = Some(Instant::now());
                self.failing = false;
            }
            RefreshEvent::Failed => self.failing = true,
        }
    }

    fn freshness(&self) -> Freshness {
        let secs_ago = self.last_verified.map(|t| t.elapsed().as_secs());
        match (self.failing, secs_ago) {
            (true, secs_ago) => Freshness::Stale { secs_ago },
            (false, Some(secs_ago)) => Freshness::Fresh { secs_ago },
            (false, None) => Freshness::Initial,
        }
    }
}

async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut maps: BTreeMap<String, Map>,
    map_issues: BTreeMap<String, u64>,
    mut updates: UnboundedReceiver<(String, RefreshEvent)>,
) -> Result<()> {
    let mut refresh = RefreshState {
        last_verified: None,
        failing: false,
    };
    loop {
        // Drain every pending poll outcome before drawing. An update swaps
        // one repo's map in the merged view; App::replace_map keeps the
        // cursor pinned to ticket identity, query and scope untouched.
        while let Ok((slug, event)) = updates.try_recv() {
            refresh.apply(&event);
            if let RefreshEvent::Updated(new_map) = event {
                maps.insert(slug, new_map);
                app.replace_map(merge_maps(&maps));
            }
        }

        let indicator = refresh.freshness().indicator();
        terminal.draw(|frame| wf::ui::draw(frame, &app, &indicator))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.handle_key(key) {
                    Outcome::Quit => return Ok(()),
                    Outcome::Refresh => {
                        // Force refresh (ctrl-r): refetch every map in
                        // place, without waiting for the poll cycles.
                        let mut failed = false;
                        for (slug, &number) in &map_issues {
                            let (owner, name) = slug.split_once('/').expect("slug is owner/name");
                            match wf::fetch::fetch_map(owner, name, number).await {
                                Ok(map) => {
                                    maps.insert(slug.clone(), map);
                                }
                                Err(_) => failed = true,
                            }
                        }
                        app.replace_map(merge_maps(&maps));
                        if failed {
                            refresh.apply(&RefreshEvent::Failed);
                            app.notice = Some("refresh failed for some projects".to_string());
                        } else {
                            refresh.apply(&RefreshEvent::Unchanged);
                            app.notice = Some("refreshed".to_string());
                        }
                    }
                    Outcome::Continue => {}
                }
            }
        }
    }
}
