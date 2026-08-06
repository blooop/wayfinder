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
//! 6. After every poll cycle reports, the same loop restores the auto-start
//!    invariant (Build 6, #19): frontier `research` tickets that have no tab
//!    get one, spawned AFK with no keystroke — see [`reconcile_autostart`].

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
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use wf::app::{App, Outcome, Scope};
use wf::autostart::{self, PollHealthByRepo};
use wf::launch::{self, Handoff, Host, Launch, MapIssues, Mode, Opened};
use wf::model::{merge_maps, Map};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{Freshness, LoadEvent, Pollers, RefreshEvent, Startup};

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
checkout you are standing in. enter runs an agent on a ticket — in its own zellij
tab where zellij is installed, in this terminal otherwise; ctrl-a spawns one
headless (needs zellij), ctrl-f/ctrl-g narrow and widen the scope,
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
    // a project. Non-checkouts and non-GitHub remotes are simply None. This
    // is local git (<10ms) and the projects cache it writes is what the first
    // frame is drawn from, so it stays ahead of the screen.
    let cwd = std::env::current_dir().context("cannot resolve the working directory")?;
    let here = projects::discover_checkout(&cwd).await;
    let cache_path =
        projects::default_cache_path().context("cannot resolve the XDG cache directory")?;
    let mut cache = ProjectsCache::load_or_default(&cache_path);
    // Accretion needs a matching forget: a checkout that has been deleted must
    // stop appearing as a launch host (and stop forcing its repo's other
    // checkout into a disambiguated session name).
    let pruned = cache.prune_missing();
    if let Some((path, slug)) = &here {
        cache.register(path.clone(), slug.clone());
        cache.save(&cache_path)?;
    } else if pruned {
        cache.save(&cache_path)?;
    }
    let repos = cache.repos();
    let sessions = launch::sessions_of(&cache.checkouts);
    // The head start (#28): the map numbers the last search found. Reading them
    // is one local file read that has already happened, so the pollers can start
    // fetching before the first frame instead of after the ~2.5 s search — which
    // is where time-to-*data* actually went. The search still runs (see
    // [`wf::refresh::spawn_discovery`]); this only decides what `wf` fetches
    // while waiting for it.
    let seed = cache.map_seed();
    let mut app = App::empty().with_projects(cache.checkouts.clone(), seed.clone());
    app.startup = Startup::seeded(&seed);

    // The screen goes up *before* any network or zellij call (#27). Everything
    // that used to run here — the map search, a serial fetch per repo, the
    // agent-tab count — now streams into a UI that is already drawn and already
    // reading keys, which is also why the progress `eprintln!`s are gone: they
    // only existed because there was no screen to say it on.
    let mut terminal = ratatui::init();
    spawn_terminal_guard();

    let (tx, updates) = mpsc::unbounded_channel();
    wf::refresh::spawn_discovery(repos, cache_path, tx.clone());
    spawn_agent_tab_count(sessions.clone(), tx.clone());

    // cwd-open focuses the project (lazygit-style). Only the map *search* can
    // say authoritatively whether this repo has a map, so the focus is handed to
    // the loop to apply the moment discovery lands — but a seeded repo can be
    // focused from the first frame, and usually is, so the picker no longer
    // opens wide and jumps a couple of seconds later. `focus` is kept either
    // way: the search still gets to overrule a seed that has gone stale.
    let focus = here.map(|(_, slug)| slug);
    if let Some(slug) = focus.as_ref().filter(|slug| seed.contains_key(*slug)) {
        app.scope = Scope::Project(slug.clone());
    }
    let result = run(&mut terminal, app, sessions, tx, updates, focus).await;
    ratatui::restore();
    if let Ending::HandedOver(parting) = result? {
        println!("wf: {parting}");
    }
    Ok(())
}

/// The signals that end `wf` from outside, with the exit code each one
/// conventionally produces (`128 + signo`).
const FATAL_SIGNALS: [(fn() -> SignalKind, i32); 3] = [
    (SignalKind::interrupt, 2),
    (SignalKind::terminate, 15),
    (SignalKind::hangup, 1),
];

/// Put the terminal back when `wf` is *killed* rather than quit.
///
/// Every ordinary exit funnels through the `ratatui::restore()` in [`main`],
/// the error paths included — but a signal bypasses it, and the process dies
/// with the tty still in raw mode. What the user then gets is a shell that
/// prints its prompt and echoes nothing they type: a terminal that looks
/// broken with no hint that `wf` is what broke it.
///
/// Key handling cannot cover this, because raw mode is precisely what stops
/// `ctrl-c` from arriving as a signal at all: anything that gets here came from
/// *outside* the TUI — a `kill`, a hangup when the session goes away, an OOM
/// kill of a neighbour's process group. That is also why this is spawned rather
/// than selected on in the event loop: the loop can be anywhere (mid-fetch,
/// mid-launch, blocked on `event::poll`) and the terminal must come back
/// regardless.
///
/// Each handler restores and exits rather than re-raising: there is nothing
/// left to unwind, and re-raising would race the restore it just did.
/// `SIGKILL` remains uncatchable, so `stty sane` stays the last resort.
fn spawn_terminal_guard() {
    for (kind, signo) in FATAL_SIGNALS {
        tokio::spawn(async move {
            // A handler that cannot be installed is not worth failing over:
            // the TUI still works, it just dies ugly on that one signal.
            let Ok(mut signal) = signal(kind()) else {
                return;
            };
            if signal.recv().await.is_some() {
                ratatui::restore();
                std::process::exit(128 + signo);
            }
        });
    }
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

/// Count the agent tabs off the path to the first frame (#27), reporting the
/// result as an event. Startup's only `zellij` traffic, and the boundary that
/// can wedge (#21) — so it must not be something the screen waits behind.
fn spawn_agent_tab_count(sessions: Vec<String>, tx: UnboundedSender<LoadEvent>) {
    tokio::spawn(async move {
        let count = launch::agent_tab_count(&sessions).await;
        let _ = tx.send(LoadEvent::AgentTabs(count));
    });
}

/// Refetch every map in place (`ctrl-r`, and after returning from a session —
/// a claim or a close landed while we were away). True if any fetch failed.
///
/// Concurrent, not sequential (#27): the fetches are independent `gh`
/// subprocesses keyed by slug, so N repos cost one round trip of wall clock
/// rather than N. This one is still awaited because both callers are explicit
/// acts whose notice has to describe what actually happened.
async fn refetch_all(maps: &mut BTreeMap<String, Map>, map_issues: &MapIssues) -> bool {
    let mut fetches = tokio::task::JoinSet::new();
    for (slug, &number) in map_issues {
        let slug = slug.clone();
        fetches.spawn(async move {
            let (owner, name) = slug.split_once('/').expect("slug is owner/name");
            let fetched = wf::fetch::fetch_map(owner, name, number).await;
            (slug, fetched)
        });
    }
    let mut failed = false;
    while let Some(joined) = fetches.join_next().await {
        match joined {
            Ok((slug, Ok(map))) => {
                maps.insert(slug, map);
            }
            // A failed fetch and a panicked task are the same fact here: this
            // repo's map did not come back.
            Ok((_, Err(_))) | Err(_) => failed = true,
        }
    }
    failed
}

/// What one launch leaves behind: the line to show for it, and whether `wf`
/// still owns a terminal to show it on.
enum LaunchReport {
    /// `wf` is still up; this is its notice line.
    Notice(String),
    /// The zellij client left for another session, so this launch was `wf`'s
    /// last act: the line is printed on the way out instead (see
    /// [`launch::Handoff::Quit`]).
    HandedOver(String),
}

/// Perform one launch (#16): give the agent somewhere to run, then hand over as
/// this host requires — suspending the TUI around `zellij attach` when `wf` owns
/// the terminal, staying up when zellij's own navigation moves the client within
/// this session, quitting when it moves the client off it, and — with no zellij
/// on the machine at all — suspending around the agent *itself*, which is then
/// `wf`'s own child and comes back here when it exits.
async fn perform_launch(
    terminal: &mut DefaultTerminal,
    launch: &Launch,
    maps: &mut BTreeMap<String, Map>,
    map_issues: &BTreeMap<String, u64>,
) -> Result<LaunchReport> {
    let host = launch::detect_host().await;
    // ctrl-a with no zellij. Refused here rather than in `App`, which decides
    // *what* to launch and is deliberately blind to the environment; refused at
    // all because #7 makes the tab the entire supervision story for a headless
    // agent, and there is no tab here to hold one.
    if launch.mode == Mode::Afk && host == Host::NoZellij {
        return Ok(LaunchReport::Notice(
            "no zellij — afk needs a tab; enter runs this ticket here instead".to_string(),
        ));
    }
    let (opened, handoff) = match launch::execute(launch, &host).await {
        Ok(result) => result,
        Err(err) => return Ok(LaunchReport::Notice(format!("launch failed: {err}"))),
    };
    let verb = opened.verb();
    match handoff {
        Handoff::Stay => Ok(LaunchReport::Notice(format!(
            "{verb} {}",
            launch.describe()
        ))),
        Handoff::Quit => Ok(LaunchReport::HandedOver(format!(
            "{verb} {} — run `wf` in that session to pick another ticket",
            launch.describe()
        ))),
        Handoff::Suspend { argv, cwd } => {
            let (program, args) = argv.split_first().expect("a handoff argv is non-empty");
            // What was handed over decides what "back" means: a whole session,
            // when the zellij client was free to roam its tabs, and this one
            // ticket when the child *was* the agent.
            let from = match &opened {
                Opened::Tab(_) => launch.session.clone(),
                Opened::Direct => launch.key().to_string(),
            };
            suspend(terminal)?;
            let status = tokio::process::Command::new(program)
                .args(args)
                .current_dir(cwd)
                .status()
                .await;
            resume(terminal)?;
            // Away for a while: the tracker moved (a claim, maybe a close).
            refetch_all(maps, map_issues).await;
            Ok(LaunchReport::Notice(match status {
                Ok(_) => format!("back from {from}"),
                Err(err) => format!("could not run {from}: {err}"),
            }))
        }
    }
}

/// Restore the auto-start invariant — *every frontier `research` ticket has a
/// tab* (#19, spec'd by #18) — and report what changed, if anything.
///
/// Called once per **poll report**, not once per event-loop turn: the decision
/// itself is pure and cheap, but feeding it costs a `zellij` subprocess per
/// session, and the frontier only moves when a poll lands. Hence
/// [`autostart::any_candidate`] first — a type/status/health filter with no IO —
/// so the tab strip is read only when some ticket could plausibly need a tab.
///
/// Create-only, and deduped on tab existence rather than on the claim, so this
/// is safe to run every cycle: a ticket whose tab already exists (running, or an
/// EXITED corpse) is left alone, and restarting `wf` does not double-spawn.
async fn reconcile_autostart(
    app: &mut App,
    health: &PollHealthByRepo,
    sessions: &[String],
) -> Option<String> {
    if !autostart::any_candidate(&app.map.tickets, health) {
        return None;
    }
    let tabs = launch::tabs_by_session(sessions).await;
    let launches = autostart::reconcile(
        &app.map.tickets,
        &app.checkouts,
        &app.map_issues,
        &tabs,
        health,
    );
    if launches.is_empty() {
        return None;
    }

    let host = launch::detect_host().await;
    let mut started = Vec::new();
    let mut failed = 0usize;
    for launch in &launches {
        match autostart::start(launch, &host).await {
            Ok(_) => started.push(launch.key().to_string()),
            Err(_) => failed += 1,
        }
    }
    // Tabs appeared without a keystroke: recount so the AFK slot agrees with
    // the tab bar (#7 — the tab *is* the supervision, so the count must be
    // honest the moment it changes).
    app.agent_tabs = launch::agent_tab_count(sessions).await;

    match (started.is_empty(), failed) {
        (true, _) => Some("auto-start failed".to_string()),
        (false, 0) => Some(format!("auto-started {}", started.join(", "))),
        (false, n) => Some(format!("auto-started {} ({n} failed)", started.join(", "))),
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

/// Why the event loop ended — the two ways `wf` gives the terminal back.
enum Ending {
    /// The user quit.
    Quit,
    /// A launch moved the zellij client to another session, so `wf` handed the
    /// terminal over; this line is printed once the terminal is restored,
    /// because by then there is no TUI left to show a notice in.
    HandedOver(String),
}

/// The event loop. It starts with **no data at all** (#27): the maps, which
/// repos even have maps, and the agent-tab count all arrive as [`LoadEvent`]s
/// while the screen is already up and answering keys.
///
/// `focus` is the cwd checkout's repo slug, if `wf` was run inside one — held
/// as an `Option` that is *taken*, so the lazygit-style focus can be applied at
/// most once, when discovery makes it answerable.
async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    sessions: Vec<String>,
    tx: UnboundedSender<LoadEvent>,
    mut updates: UnboundedReceiver<LoadEvent>,
    mut focus: Option<String>,
) -> Result<Ending> {
    let mut maps: BTreeMap<String, Map> = BTreeMap::new();
    // The cached seed starts fetching immediately (#28); the search's answer
    // reconciles this set rather than adding to it, so a number that moved is
    // corrected in the task actually doing the fetching, not just in the state
    // `enter` reads.
    let mut pollers = Pollers::new();
    pollers.reconcile(&app.map_issues, &tx);
    let mut refresh = RefreshState {
        last_verified: None,
        failing: false,
    };
    // Per-repo poll health, the freshness gate auto-start reads (#19). Starts
    // empty, which reads as `Awaiting` for every repo — that is what puts the
    // first reconcile on the first poll tick rather than at startup.
    let mut health = PollHealthByRepo::new();
    loop {
        // Drain everything that landed before drawing. A fetch swaps one repo's
        // map in the merged view; App::replace_map keeps the cursor pinned to
        // ticket identity, query and scope untouched.
        let mut polled = false;
        while let Ok(event) = updates.try_recv() {
            match event {
                LoadEvent::Discovered(map_issues) => {
                    // Reconciling the pollers *is* the load for every repo the
                    // seed did not already cover: each new poller's first cycle
                    // fetches unconditionally, so those maps are all in flight at
                    // once and each lands on screen as it arrives. Repos the seed
                    // got right keep polling untouched — and keep their ETag.
                    pollers.reconcile(&map_issues, &tx);
                    app.startup.searched(&map_issues);
                    if let Some(slug) = focus.take() {
                        if map_issues.contains_key(&slug) {
                            app.scope = Scope::Project(slug);
                        } else {
                            // The seed may have focused this repo a moment ago on
                            // a map that is gone; the search is the authority, so
                            // the focus goes back.
                            if app.scope == Scope::Project(slug.clone()) {
                                app.scope = Scope::All;
                            }
                            app.notice = Some(format!(
                                "{slug} has no wayfinder:map — showing all projects"
                            ));
                        }
                    }
                    // Maps the search dropped must stop being rendered as well as
                    // stop being polled — their rows are as stale as their poller.
                    maps.retain(|slug, _| map_issues.contains_key(slug));
                    app.map_issues = map_issues;
                    app.replace_map(merge_maps(&maps));
                }
                // Discovery retries, so this is a status report and not an end
                // state: `wf` stays on screen and recovers when the search does.
                LoadEvent::SearchFailed => {
                    app.notice = Some("map search failed — retrying".to_string());
                }
                LoadEvent::Fetched { repo, outcome } => {
                    refresh.apply(&outcome);
                    health.record(&repo, &outcome);
                    app.startup.record_arrival(&repo);
                    polled = true;
                    if let RefreshEvent::Updated(new_map) = outcome {
                        maps.insert(repo, new_map);
                        app.replace_map(merge_maps(&maps));
                    }
                }
                LoadEvent::AgentTabs(count) => app.agent_tabs = count,
            }
        }
        // Reconcile on poll reports only, so the cadence is the poller's (~4s)
        // and not this loop's 250ms keyboard tick.
        if polled {
            if let Some(notice) = reconcile_autostart(&mut app, &health, &sessions).await {
                app.notice = Some(notice);
            }
        }

        let indicator = refresh.freshness().indicator();
        terminal.draw(|frame| wf::ui::draw(frame, &app, &indicator))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let scope_before = app.scope.clone();
                let outcome = app.handle_key(key);
                // A scope the user chose while the load was still out is theirs
                // to keep: the pending cwd focus is dropped rather than applied
                // over their `ctrl-g` when discovery lands a moment later.
                if app.scope != scope_before {
                    focus = None;
                }
                match outcome {
                    Outcome::Quit => return Ok(Ending::Quit),
                    Outcome::Refresh => {
                        // Force refresh (ctrl-r): refetch every map in
                        // place, without waiting for the poll cycles, and
                        // recount the agent tabs while we are at it.
                        let failed = refetch_all(&mut maps, &app.map_issues).await;
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
                        let map_issues = app.map_issues.clone();
                        let notice = match perform_launch(terminal, &launch, &mut maps, &map_issues)
                            .await?
                        {
                            LaunchReport::Notice(notice) => notice,
                            // Nothing after this can be seen from here: the
                            // client is in another session now.
                            LaunchReport::HandedOver(parting) => {
                                return Ok(Ending::HandedOver(parting))
                            }
                        };
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
