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
//! 5. `enter` / `ctrl-a` launch the picked ticket through the Build 4 seam
//!    (#16): this loop is the only place that may suspend the TUI, so it
//!    performs what [`wf::launch`] decides.

use std::collections::BTreeMap;
use std::io::stdout;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedReceiver;

use wf::app::{App, Outcome, Scope};
use wf::launch::{self, Handoff, Launch, TabOutcome};
use wf::model::{merge_maps, Map};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{Freshness, Poller, RefreshEvent};

/// Everything `wf`'s argv can mean. Only one shape opens the TUI; the others
/// answer on a stream and exit, touching neither the terminal, `gh`, nor
/// zellij — which is what makes `wf --version` a usable packaging smoke test
/// on a CI runner that has no tty, no auth and no multiplexer.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Tui,
    /// Print to stdout, exit 0.
    Print(String),
    /// Print to stderr, exit 2.
    Reject(String),
}

const USAGE: &str = "\
wf — the multi-project wayfinder manager TUI

usage: wf [--version | --help]

With no arguments: opens the picker over every mapped project, focused on the
checkout you are standing in. enter launches (or focuses) a ticket's agent tab,
ctrl-a spawns it headless, ctrl-f/ctrl-g narrow and widen the scope,
ctrl-r refreshes, esc quits.";

/// Parse argv (without the program name). `wf` takes at most one argument, and
/// anything it does not recognise is rejected rather than ignored, so a typo
/// can never silently open the TUI instead.
///
/// Matched on the argument *slice* rather than on a pair of `next()` calls: a
/// `(first, second)` tuple can hold `(None, Some(_))` — no first argument but a
/// second one — which argv cannot produce, and absorbing it with a wildcard
/// would also let a future third position slip through unhandled. The three
/// slice patterns below are exhaustive over every possible argv with no
/// wildcard, so the compiler is what keeps this total.
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Invocation::Tui,
        [flag] => match flag.as_str() {
            "--version" | "-V" => Invocation::Print(format!("wf {}", env!("CARGO_PKG_VERSION"))),
            "--help" | "-h" => Invocation::Print(USAGE.to_string()),
            other => Invocation::Reject(format!("wf: unknown argument {other:?}\n{USAGE}")),
        },
        [_, second, ..] => Invocation::Reject(format!(
            "wf: too many arguments (unexpected {second:?})\n{USAGE}"
        )),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match parse_args(std::env::args().skip(1)) {
        Invocation::Print(text) => {
            println!("{text}");
            return Ok(());
        }
        Invocation::Reject(text) => {
            eprintln!("{text}");
            std::process::exit(2);
        }
        Invocation::Tui => {}
    }

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

    let mut app = App::new(merge_maps(&maps))
        .with_projects(cache.checkouts.clone(), map_issues.clone());
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

    // Agent tabs already open from earlier sessions: the AFK line is honest
    // from the first frame (#7 — the tab is the supervision, and it outlives
    // any one `wf`).
    let sessions = launch::sessions_of(&cache.checkouts);
    app.agent_tabs = launch::agent_tab_count(&sessions).await;

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, app, maps, map_issues, sessions, updates).await;
    ratatui::restore();
    result
}

/// Leave the TUI's grip on the terminal so a child process can own it. Paired
/// with [`resume`] — the child is *never* `exec`ed, so detaching from it comes
/// back here (#5).
fn suspend(terminal: &mut DefaultTerminal) -> Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Retake the terminal after the child exited, and force a full redraw.
fn resume(terminal: &mut DefaultTerminal) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

/// Refetch every map in place (`ctrl-r`, and after returning from a session —
/// a claim or a close landed while we were away). True if any fetch failed.
async fn refetch_all(maps: &mut BTreeMap<String, Map>, map_issues: &BTreeMap<String, u64>) -> bool {
    let mut failed = false;
    for (slug, &number) in map_issues {
        let (owner, name) = slug.split_once('/').expect("slug is owner/name");
        match wf::fetch::fetch_map(owner, name, number).await {
            Ok(map) => {
                maps.insert(slug.clone(), map);
            }
            Err(_) => failed = true,
        }
    }
    failed
}

/// Perform one launch (#16): create-or-focus the tab, then hand over as this
/// host requires — suspending the TUI around `zellij attach` when `wf` owns
/// the terminal, staying up when zellij's own navigation does the moving.
/// Returns the notice to show.
async fn perform_launch(
    terminal: &mut DefaultTerminal,
    launch: &Launch,
    maps: &mut BTreeMap<String, Map>,
    map_issues: &BTreeMap<String, u64>,
) -> Result<String> {
    let host = launch::detect_host();
    let (tab, handoff) = match launch::execute(launch, &host).await {
        Ok(result) => result,
        Err(err) => return Ok(format!("launch failed: {err}")),
    };
    let verb = match tab {
        TabOutcome::Created => "started",
        TabOutcome::Existed => "focused",
    };
    match handoff {
        Handoff::Stay => Ok(format!("{verb} {}", launch.describe())),
        Handoff::Suspend(argv) => {
            let (program, args) = argv.split_first().expect("attach argv is non-empty");
            suspend(terminal)?;
            let status = tokio::process::Command::new(program).args(args).status().await;
            resume(terminal)?;
            // Away for a while: the tracker moved (a claim, maybe a close).
            refetch_all(maps, map_issues).await;
            Ok(match status {
                Ok(_) => format!("back from {}", launch.session),
                Err(err) => format!("could not attach to {}: {err}", launch.session),
            })
        }
    }
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
    sessions: Vec<String>,
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
                        // place, without waiting for the poll cycles, and
                        // recount the agent tabs while we are at it.
                        let failed = refetch_all(&mut maps, &map_issues).await;
                        app.replace_map(merge_maps(&maps));
                        app.agent_tabs = launch::agent_tab_count(&sessions).await;
                        if failed {
                            refresh.apply(&RefreshEvent::Failed);
                            app.notice = Some("refresh failed for some projects".to_string());
                        } else {
                            refresh.apply(&RefreshEvent::Unchanged);
                            app.notice = Some("refreshed".to_string());
                        }
                    }
                    Outcome::Launch(launch) => {
                        let notice =
                            perform_launch(terminal, &launch, &mut maps, &map_issues).await?;
                        app.replace_map(merge_maps(&maps));
                        app.notice = Some(notice);
                        // The new tab counts towards the AFK line; this also
                        // reaps tabs closed by hand since the last check.
                        app.agent_tabs = launch::agent_tab_count(&sessions).await;
                    }
                    Outcome::Continue => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_opens_the_tui() {
        assert_eq!(parse_args(argv(&[])), Invocation::Tui);
    }

    #[test]
    fn version_prints_the_crate_version_and_nothing_else() {
        let expected = Invocation::Print(format!("wf {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(parse_args(argv(&["--version"])), expected);
        assert_eq!(parse_args(argv(&["-V"])), expected);
    }

    #[test]
    fn help_prints_the_usage() {
        assert_eq!(
            parse_args(argv(&["--help"])),
            Invocation::Print(USAGE.to_string())
        );
    }

    #[test]
    fn an_unknown_flag_is_rejected_not_ignored() {
        match parse_args(argv(&["--versoin"])) {
            Invocation::Reject(message) => assert!(message.contains("--versoin")),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_second_argument_is_rejected() {
        match parse_args(argv(&["--version", "--help"])) {
            Invocation::Reject(message) => assert!(message.contains("too many")),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }
}
