//! `wf` binary — the ticket selector (#26/#34).
//!
//! 1. If the cwd is inside a git checkout with a GitHub `origin`, register
//!    it in the per-machine cache (`~/.cache/wf/projects.json`) — explicit
//!    use *is* the registration act (the zoxide model, per #4).
//! 2. The screen goes up before any network call (#27), with the cached map
//!    numbers (#28) already being fetched.
//! 3. One `wayfinder:map` label search reconciles that set; every map streams
//!    in as it lands. `ctrl-r` asks again. Nothing polls: `wf` is on screen for
//!    seconds and restarts warm in ~0.6 s.
//! 4. Inside a checkout whose repo has a map, the screen opens focused on
//!    it (`Scope::Project`, repo column dropped); `ctrl-g` widens to all
//!    projects, `ctrl-f` re-focuses.
//! 5. `enter` picks a ticket, and that is the end of `wf`: the loop returns,
//!    the terminal is restored, and this process is replaced by the agent
//!    ([`wf::launch::Launch::exec`]). The one ordering that matters —
//!    restore *then* exec — is the two statements at the bottom of [`main`],
//!    because after the exec there is no `wf` left to restore anything.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use wf::app::{App, Outcome, Scope};
use wf::launch::{Launch, MapIssues};
use wf::model::{merge_maps, Map};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{LoadEvent, Loaders, MapFetch, Startup};

/// How long the loop waits on a keypress before redrawing — the cadence at
/// which streamed load events reach the screen.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// Everything `wf`'s argv can mean. Only one shape opens the TUI; the others
/// answer on a stream and exit, touching neither the terminal nor `gh` — which
/// is what makes `wf --version` a usable packaging smoke test on a CI runner
/// with no tty and no auth.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Tui,
    /// Print to stdout, exit 0.
    Print(String),
    /// Print to stderr, exit 2.
    Reject(String),
}

const USAGE: &str = "\
wf — the multi-project wayfinder ticket selector

usage: wf [--version | --help]

With no arguments: opens the picker over every mapped project, focused on the
checkout you are standing in. enter runs an agent on the picked ticket, in that
checkout, replacing wf — so wf is gone by the time the agent draws. ctrl-f and
ctrl-g narrow and widen the scope, ctrl-r refetches, esc quits.";

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
    // stop offering itself as somewhere an agent could run.
    let pruned = cache.prune_missing();
    if let Some((path, slug)) = &here {
        cache.register(path.clone(), slug.clone());
        cache.save(&cache_path)?;
    } else if pruned {
        cache.save(&cache_path)?;
    }
    let repos = cache.repos();
    // The head start (#28): the map numbers the last search found. Reading them
    // is one local file read that has already happened, so the fetches can start
    // before the first frame instead of after the ~2.5 s search — which is where
    // time-to-*data* actually went. The search still runs (see
    // [`wf::refresh::spawn_discovery`]); this only decides what `wf` fetches
    // while waiting for it.
    let seed = cache.map_seed();
    let mut app = App::empty().with_projects(cache.checkouts.clone(), seed.clone());
    app.startup = Startup::seeded(&seed);

    // The screen goes up *before* any network call (#27). Everything that used
    // to run here — the map search, a serial fetch per repo — now streams into a
    // UI that is already drawn and already reading keys.
    let mut terminal = ratatui::init();
    spawn_terminal_guard();

    let (tx, updates) = mpsc::unbounded_channel();
    wf::refresh::spawn_discovery(repos, cache_path, tx.clone());

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
    let ending = run(&mut terminal, app, tx, updates, focus).await;

    // The one ordering that matters, and the reason the exec is here rather
    // than in the loop: the terminal must be back in the shell's hands before
    // the process image is replaced, because afterwards there is no `wf` left
    // to put it back.
    ratatui::restore();
    match ending? {
        Ending::Quit => Ok(()),
        // Only ever returns an error: on success this process *is* the agent.
        Ending::Handover(launch) => Err(launch.exec()),
    }
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
/// than selected on in the event loop: the loop can be anywhere and the terminal
/// must come back regardless.
///
/// These handlers die with the process image at the `exec`, which is correct:
/// by then the terminal has already been restored and the agent owns it, so a
/// `SIGINT` belongs to the agent and not to a `wf` that no longer exists.
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

/// Refetch every map in place (`ctrl-r`). True if any fetch failed.
///
/// Concurrent, not sequential (#27): the fetches are independent `gh`
/// subprocesses keyed by slug, so N repos cost one round trip of wall clock
/// rather than N. Awaited rather than streamed, unlike the initial load,
/// because it is an explicit act whose notice has to describe what happened.
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

/// Why the event loop ended — the two ways `wf` gives the terminal back.
///
/// A sum rather than "quit, plus maybe a launch on the side": these are the
/// only two exits, they are mutually exclusive, and the second carries exactly
/// what the caller needs to finish the job. Nothing here performs the launch,
/// because performing it means the terminal must already be restored — and this
/// value is what carries that requirement out to where it can be met.
enum Ending {
    /// The user quit.
    Quit,
    /// A ticket was picked. `wf`'s last act is to become its agent.
    Handover(Launch),
}

/// The event loop. It starts with **no data at all** (#27): which repos even
/// have maps, and the maps themselves, arrive as [`LoadEvent`]s while the screen
/// is already up and answering keys.
///
/// `focus` is the cwd checkout's repo slug, if `wf` was run inside one — held
/// as an `Option` that is *taken*, so the lazygit-style focus can be applied at
/// most once, when discovery makes it answerable.
async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    tx: UnboundedSender<LoadEvent>,
    mut updates: UnboundedReceiver<LoadEvent>,
    mut focus: Option<String>,
) -> Result<Ending> {
    let mut maps: BTreeMap<String, Map> = BTreeMap::new();
    // The cached seed starts fetching immediately (#28); the search's answer
    // reconciles this set rather than adding to it, so a number that moved is
    // corrected in the task actually doing the fetching, not just in the state
    // `enter` reads.
    let mut loaders = Loaders::new();
    loaders.reconcile(&app.map_issues, &tx);
    loop {
        // Drain everything that landed before drawing. A fetch swaps one repo's
        // map in the merged view; App::replace_map keeps the cursor pinned to
        // ticket identity, query and scope untouched.
        while let Ok(event) = updates.try_recv() {
            match event {
                LoadEvent::Discovered(map_issues) => {
                    // Reconciling the loaders *is* the load for every repo the
                    // seed did not already cover: those fetches are all in
                    // flight at once and each lands on screen as it arrives.
                    loaders.reconcile(&map_issues, &tx);
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
                    // stop being fetched — their rows are as stale as their load.
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
                    app.startup.record_arrival(&repo);
                    match outcome {
                        MapFetch::Loaded(new_map) => {
                            maps.insert(repo, new_map);
                            app.replace_map(merge_maps(&maps));
                        }
                        // Nothing polls any more, so a failed load is not a blip
                        // that the next cycle will paper over — it is the final
                        // word on that repo until someone asks again. Say so on
                        // the line rather than leaving a silently missing map.
                        MapFetch::Failed => {
                            app.notice = Some(format!("{repo}: map fetch failed — ctrl-r retries"));
                        }
                    }
                }
            }
        }

        terminal.draw(|frame| wf::ui::draw(frame, &app))?;
        if event::poll(TICK)? {
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
                        let failed = refetch_all(&mut maps, &app.map_issues).await;
                        app.replace_map(merge_maps(&maps));
                        app.notice = Some(if failed {
                            "refresh failed for some projects".to_string()
                        } else {
                            "refreshed".to_string()
                        });
                    }
                    // Nothing after this can be drawn: the caller restores the
                    // terminal and this process becomes the agent.
                    Outcome::Launch(launch) => return Ok(Ending::Handover(launch)),
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
