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
//! 4. Inside a checkout, the screen opens **on that project** and nowhere
//!    else; run outside one and it opens on the project list, most recently
//!    used first. Both are drawn from the cache, so neither waits on the
//!    search — and `←` walks between them.
//! 5. `enter` picks a ticket, and that is the end of `wf`: the loop returns,
//!    the terminal is restored, and this process is replaced by the agent
//!    ([`wf::launch::Launch::exec`]). The one ordering that matters —
//!    restore *then* exec — is the two statements at the bottom of [`main`],
//!    because after the exec there is no `wf` left to restore anything.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use wf::app::{App, Outcome};
use wf::launch::{Agent, Launch};
use wf::model::{Map, MapId};
use wf::projects::{self, ProjectsCache};
use wf::reap;
use wf::refresh::{LoadEvent, Loaders, MapFetch, Startup};
use wf::skills;

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
    /// Report where the bundled skills are and whether they are linked.
    SkillsReport,
    /// Link the bundled skills into Claude Code's and Codex's personal skills
    /// directories.
    SkillsInstall,
    /// Remove the workspaces whose tickets are closed. `yes` skips the prompt;
    /// `insist` also reaps workspaces holding work that is not pushed anywhere,
    /// which is what a devcontainer that dirties its own checkout on every
    /// build leaves behind.
    Reap {
        yes: bool,
        insist: bool,
    },
    /// Print to stdout, exit 0.
    Print(String),
    /// Print to stderr, exit 2.
    Reject(String),
}

const USAGE: &str = "\
wf — the multi-project wayfinder ticket selector

usage: wf [--version | --help]
       wf skills [install]
       wf reap [-y] [-f]

With no arguments: opens on the project you are standing in, or on the list of
every registered project — most recently used first — when you are not standing
in one. enter selects a project, or runs an agent on the picked ticket in that
project's checkout, replacing wf — so wf is gone by the time the agent draws.
Left backs out, ctrl-r refetches, esc quits.

wf skills          report which prompt each route would actually run
wf skills install  link this build's skills into ~/.claude/skills and ~/.codex/skills
wf reap            remove the workspaces whose work is over — a closed ticket,
                   or an open one whose PR merged with nothing still in flight
                   (-y to skip the prompt). Warns about the ones it suspects
                   but will not delete: a ticket whose PRs all closed unmerged,
                   or one nobody claimed that nothing came of. Keeps anything
                   running or holding work that is not pushed anywhere; -f
                   reaps the unpushed ones too, naming what it discards.
                   Needs dl 0.0.21 or newer.

The skills wf execs ship in this package, so they update with it. `install`
links both agents' skills directories at copies kept beside them; every launch
brings its selected agent's copy back in step. Set WF_SKILLS_DIR to install a
checkout's skills instead, while you are editing them.";

/// Parse argv (without the program name). `wf` takes at most one argument, and
/// anything it does not recognise is rejected rather than ignored, so a typo
/// can never silently open the TUI instead.
///
/// Matched on the argument *slice* rather than on a pair of `next()` calls: a
/// `(first, second)` tuple can hold `(None, Some(_))` — no first argument but a
/// second one — which argv cannot produce, and absorbing it with a wildcard
/// would also let a future third position slip through unhandled. The four
/// slice patterns below are exhaustive over every possible argv with no
/// wildcard, so the compiler is what keeps this total.
///
/// `skills` is the one argument that takes a second word, so it is matched
/// before the too-many-arguments arm rather than inside it — a two-word argv
/// is only ever an error for the *other* first words.
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Invocation::Tui,
        [flag] => match flag.as_str() {
            "--version" | "-V" => Invocation::Print(format!("wf {}", env!("CARGO_PKG_VERSION"))),
            "--help" | "-h" => Invocation::Print(USAGE.to_string()),
            "skills" => Invocation::SkillsReport,
            "reap" => Invocation::Reap {
                yes: false,
                insist: false,
            },
            other => Invocation::Reject(format!("wf: unknown argument {other:?}\n{USAGE}")),
        },
        [first, second] if first == "skills" => match second.as_str() {
            "install" => Invocation::SkillsInstall,
            other => Invocation::Reject(format!(
                "wf: unknown skills subcommand {other:?} (expected `install`)\n{USAGE}"
            )),
        },
        [first, rest @ ..] if first == "reap" => parse_reap(rest),
        [_, second, ..] => Invocation::Reject(format!(
            "wf: too many arguments (unexpected {second:?})\n{USAGE}"
        )),
    }
}

/// Parse `wf reap`'s flags, which unlike every other `wf` argument may be
/// combined and given in any order.
///
/// Rejecting an unknown flag rather than ignoring it matters more here than
/// anywhere else in this parser: the two flags waive a confirmation and a
/// safety guard, so a typo that were quietly dropped would leave someone
/// believing they had asked for something they had not — and a mistyped `-y`
/// that silently opened a prompt is a much better outcome than a mistyped `-f`
/// that silently did nothing.
fn parse_reap(flags: &[String]) -> Invocation {
    let (mut yes, mut insist) = (false, false);
    for flag in flags {
        match flag.as_str() {
            "-y" | "--yes" => yes = true,
            "-f" | "--force" => insist = true,
            other => {
                return Invocation::Reject(format!(
                    "wf: unknown reap argument {other:?} (expected `-y` or `-f`)\n{USAGE}"
                ))
            }
        }
    }
    Invocation::Reap { yes, insist }
}

/// `wf skills` and `wf skills install`. Both resolve the bundle and the target
/// the same way, so a report and an install can never disagree about which
/// files they are talking about.
///
/// Exit codes matter here: this is the command a setup script runs, so a
/// blocked or missing skill exits non-zero rather than reporting itself in
/// prose and calling that success.
fn run_skills(install: bool) -> Result<()> {
    use std::fmt::Write;

    let bundle = skills::Bundle::resolve()?;
    let mut out = String::new();
    let mut blocked = false;
    for agent in Agent::all() {
        let target = skills::Target::resolve(agent)?;
        let _ = writeln!(out, "{}", agent.label());
        if install {
            // Before linking, not after: a swept name could be one this build
            // ships under a new spelling, and clearing the old link first keeps
            // the two steps from racing over the same directory entry.
            let swept = skills::sweep(&bundle, &target)?;
            let done = skills::install(&bundle, &target)?;
            for link in &swept {
                let _ = writeln!(
                    out,
                    "  {:<15} removed — a skill an older wf shipped and this one does not",
                    link.file_name()
                        .unwrap_or(link.as_os_str())
                        .to_string_lossy()
                );
            }
            for (name, outcome) in &done {
                let line = match outcome {
                    skills::Outcome::AlreadyCurrent => "already current".to_string(),
                    skills::Outcome::Refreshed => {
                        "refreshed — the prompt behind the link is now this build's".to_string()
                    }
                    skills::Outcome::Linked { was: None } => "linked".to_string(),
                    skills::Outcome::Linked { was: Some(old) } => {
                        format!("relinked (was {})", old.display())
                    }
                    skills::Outcome::Blocked => format!(
                        "BLOCKED — {} is a real directory, not a link. Remove it \
                         (chezmoi may own it) and run this again",
                        target.links().join(name).display()
                    ),
                    skills::Outcome::NotInBundle => {
                        "NOT IN BUNDLE — this build shipped without it".to_string()
                    }
                };
                let _ = writeln!(out, "  {name:<15} {line}");
            }
            blocked |= done.iter().any(|(_, outcome)| {
                matches!(
                    outcome,
                    skills::Outcome::Blocked | skills::Outcome::NotInBundle
                )
            });
            out.push('\n');
        }
        out.push_str(&skills::report(&bundle, &target));
        out.push('\n');
    }
    emit(&out);
    if blocked {
        std::process::exit(1);
    }
    Ok(())
}

/// `wf reap`: remove the workspaces whose nodes the tracker calls finished.
///
/// The division of labour is the one the launch already draws — `dl` owns the
/// containers, `wf` owns the tickets — so this asks `dl` what exists, asks the
/// tracker what has become of those nodes, prints the plan, and hands the
/// finished ones back to `dl`. No terminal is taken: this is a stream command
/// like `wf skills`, not a second TUI.
///
/// The plan is printed **before** the prompt and includes what is being kept,
/// because a workspace someone expected to go and that stayed is the thing they
/// most need told about, and a reason they disagree with ("still running" when
/// they thought they had stopped it) is only actionable while no is an answer.
///
/// `warn` rows are the same argument pointed the other way: workspaces `wf`
/// suspects are dead weight on evidence too weak to act on — a superseded
/// ticket, or a node nothing has come of. They are printed and never counted
/// into the prompt, because the only safe thing to do with a suspicion is say
/// it out loud.
async fn run_reap(yes: bool, insist: bool) -> Result<()> {
    use std::collections::BTreeSet;
    use std::fmt::Write;
    use std::io::Write as _;

    let workspaces = reap::workspaces().await?;
    let nodes: BTreeSet<reap::Node> = workspaces.iter().filter_map(reap::node_of).collect();
    if nodes.is_empty() {
        emit("no wayfinder workspaces on this machine — nothing to reap\n");
        return Ok(());
    }
    let known = reap::node_facts(&nodes).await?;
    let verdicts = reap::plan(&workspaces, &known, insist);

    // The deletion set is asked for rather than re-derived here: `reap::doomed`
    // is the one definition of what goes, so a warning row cannot become a
    // deletion by way of a partition written twice.
    let doomed = reap::doomed(&verdicts);
    // Grouped rather than in listing order, and in this order: what stays,
    // then what `wf` is uneasy about, then — last, immediately above the
    // prompt — what the y/N is actually about.
    let mut out = String::new();
    for label in ["keep", "warn", "reap"] {
        for verdict in &verdicts {
            let row = match verdict {
                reap::Verdict::Keep { .. } => "keep",
                reap::Verdict::Warn { .. } => "warn",
                reap::Verdict::Reap { .. } => "reap",
            };
            if row == label {
                let _ = writeln!(out, "  {label}  {}  ({})", verdict.id(), verdict.reason());
            }
        }
    }
    emit(&out);
    if doomed.is_empty() {
        emit("nothing to reap\n");
        return Ok(());
    }

    if !yes {
        emit(&format!("\ndelete {} workspace(s)? [y/N] ", doomed.len()));
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("cannot read the answer")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            emit("aborted\n");
            return Ok(());
        }
    }

    // One at a time, reporting each: `dl <ws> rm` tears down a container, and a
    // failure part-way through leaves a set the next run has to be able to make
    // sense of. Failures are collected rather than propagated at the first one,
    // so a single wedged workspace does not strand the rest.
    let mut failed = Vec::new();
    for verdict in &doomed {
        match reap::remove(verdict.id(), insist).await {
            Ok(()) => emit(&format!("removed {}\n", verdict.id())),
            Err(e) => {
                emit(&format!("could not remove {}: {e}\n", verdict.id()));
                failed.push(verdict.id().to_string());
            }
        }
    }
    if !failed.is_empty() {
        bail!("{} workspace(s) could not be removed", failed.len());
    }
    Ok(())
}

/// Write a whole report to stdout, tolerating a reader that has gone away.
///
/// `println!` panics on a closed pipe: Rust ignores `SIGPIPE`, so the write
/// returns `EPIPE` and the macro unwraps it. `wf skills | head` is an ordinary
/// thing to type and a panic is an absurd answer to it — the reader stopped
/// listening, which is not this program's problem to report. One write of one
/// string rather than a line at a time, so there is a single place for that to
/// be true.
fn emit(text: &str) {
    use std::io::Write;
    let _ = std::io::stdout().write_all(text.as_bytes());
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
        // Both skills paths answer on a stream and exit, like `--version`:
        // no terminal, no `gh`, so they stay usable in a package test and in
        // whatever script installs the tool.
        Invocation::SkillsReport => {
            run_skills(false)?;
            return Ok(());
        }
        Invocation::SkillsInstall => {
            run_skills(true)?;
            return Ok(());
        }
        Invocation::Reap { yes, insist } => {
            run_reap(yes, insist).await?;
            return Ok(());
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
    let mut app = App::empty()
        .with_checkouts(cache.checkouts.clone())
        .with_sessions(cache.sessions.clone());
    app.open_maps = seed.clone();
    app.startup = Startup::seeded(&seed);

    // The screen goes up *before* any network call (#27). Everything that used
    // to run here — the map search, a serial fetch per repo — now streams into a
    // UI that is already drawn and already reading keys.
    let mut terminal = ratatui::init();
    spawn_terminal_guard();

    let (tx, updates) = mpsc::unbounded_channel();
    let discovery = wf::refresh::spawn_discovery(repos, cache_path.clone(), tx.clone());

    // cwd-open enters the project, on the first frame and unconditionally.
    //
    // It used to wait: the focus was only applied to a repo the cached seed
    // already knew had a map, and otherwise handed to the loop to apply when
    // discovery landed, because a focused repo with no maps rendered an empty
    // screen. It cannot now — a project's screen leads with the project's own
    // row, which is a place to stand whether or not anything has been filed in
    // the repo, let alone fetched. So the level is decided by one local `git`
    // call, before any network call, and nothing arriving later moves it.
    if let Some((_, slug)) = &here {
        app.enter(slug);
    }
    let ending = run(&mut terminal, app, discovery, tx, updates).await;

    // The one ordering that matters, and the reason the exec is here rather
    // than in the loop: the terminal must be back in the shell's hands before
    // the process image is replaced, because afterwards there is no `wf` left
    // to put it back.
    //
    // `show_cursor` is part of that and not a flourish. Nothing in the picker
    // ever positions a cursor, so every `Terminal::draw` writes `ESC[?25l`, and
    // the only thing that writes it back is `Terminal`'s `Drop` —
    // `ratatui::restore()` is just raw-mode-off plus leave-alternate-screen.
    // On the quit path `Drop` runs at the end of `main`; on the handover path
    // `exec` replaces the image first and it never runs. So the agent would
    // inherit an invisible cursor, on a terminal-global mode that outlives the
    // alternate screen. This is the line the deleted `suspend()` had.
    let _ = terminal.show_cursor();
    ratatui::restore();
    match ending? {
        Ending::Quit => Ok(()),
        Ending::Handover(launch) => {
            // The launch is a use of this project, and the last chance to say
            // so: after the exec there is no `wf` left to write anything. This
            // is what keeps the project list ordered for someone who reaches
            // their projects *through* it — opening `wf` in a checkout stamps
            // it, and for everyone else launching is the only other act that
            // means "this is what I am working on".
            //
            // Re-read before writing, exactly as the discovery task does: it
            // writes the search's findings to this same file while the picker
            // is up, so the copy loaded before the first frame is stale by
            // now, and saving it would trade this stamp for next run's head
            // start.
            //
            // Best-effort on purpose. A cache that will not write is not worth
            // refusing a launch over, and the only cost of losing this is one
            // project sitting lower in a list than it might have.
            let mut cache = ProjectsCache::load_or_default(&cache_path);
            let mut changed = cache.touch(launch.cwd());
            // And record the conversation this launch is about to start, so a
            // later run can offer the way back into it (#35).
            //
            // Written **here**, immediately before the terminal is restored
            // and the image replaced, because this is the last moment `wf`
            // exists — and written from the resolved launch rather than from
            // the picker, so what is remembered is the tree the agent actually
            // gets. A creation records nothing: it has no node to key on until
            // its skill files one.
            //
            // Best-effort like the stamp above it, and for a smaller cost: a
            // record that fails to write means the resume row is missing next
            // time, not that anything is wrong with the launch.
            if let Some(session) = launch.session() {
                cache.record_session(session);
                changed = true;
            }
            if changed {
                let _ = cache.save(&cache_path);
            }
            // The prompts the selected agent is about to run. `wf skills
            // install` links its skills directory at a *copy* of the bundle,
            // and a copy is a thing that can fall behind a `pixi global update
            // wf`. This is where it cannot: the process that refreshes it is
            // the same one that then execs the prompt, so no launch ever gets
            // ahead by even one release.
            refresh_skills(launch.agent());
            // Only ever returns an error: on success this process *is* the agent.
            Err(launch.exec())
        }
    }
}

/// Bring the installed skill copies back in step with the bundle they were
/// installed from.
///
/// Best-effort, and deliberately silent when there is nothing to do: a machine
/// with no home directory to resolve, and one that never ran
/// `wf skills install`, are not worth a word on the way into an agent. A copy
/// that could not be *written* is different — the agent is about to run a
/// prompt that is not the one that was installed — so that one is said out
/// loud.
fn refresh_skills(agent: Agent) {
    let Ok(target) = skills::Target::resolve(agent) else {
        return;
    };
    if let Err(err) = skills::refresh(&target) {
        eprintln!(
            "wf: could not refresh the {} skills: {err:#}",
            agent.label()
        );
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
    Handover(Box<Launch>),
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
    discovery: JoinHandle<()>,
    tx: UnboundedSender<LoadEvent>,
    mut updates: UnboundedReceiver<LoadEvent>,
) -> Result<Ending> {
    let mut clusters: BTreeMap<MapId, Map> = BTreeMap::new();
    // The cached seed starts fetching immediately (#28); the search's answer
    // reconciles this set rather than adding to it, so a map that closed or
    // opened is corrected in the tasks actually doing the fetching, not just
    // in the state the screen reads.
    let mut loaders = Loaders::new();
    loaders.reconcile(&app.open_maps, &tx);
    loop {
        // Drain everything that landed before drawing. A fetch swaps one map's
        // cluster; App::replace_clusters keeps the cursor pinned to row
        // identity, query and scope untouched.
        while let Ok(event) = updates.try_recv() {
            match event {
                LoadEvent::Discovered(found) => {
                    // Reconciling the loaders *is* the load for every map the
                    // seed did not already cover: those fetches are all in
                    // flight at once and each lands on screen as it arrives.
                    loaders.reconcile(&found, &tx);
                    app.startup.searched(&found);
                    // Nothing to apply here any more: the level was decided
                    // before the first frame and discovery has no say in it.
                    // A repo the search finds no map for renders its project
                    // row saying so, which is both the notice this used to
                    // post and somewhere to act on it.
                    //
                    // Maps the search dropped must stop being rendered as well as
                    // stop being fetched — their rows are as stale as their load.
                    // A map that is no longer open also stops being a *failure*:
                    // there is nothing left to have failed.
                    clusters.retain(|id, _| found.contains(id));
                    app.failed.retain(|id| found.contains(id));
                    app.open_maps = found;
                    app.replace_clusters(clusters.clone());
                }
                // Discovery retries, so this is a status report and not an end
                // state: `wf` stays on screen and recovers when the search does.
                LoadEvent::SearchFailed => {
                    app.notice = Some("map search failed — retrying".to_string());
                }
                LoadEvent::Fetched { id, outcome } => {
                    app.startup.record_arrival(&id);
                    match outcome {
                        MapFetch::Loaded(new_map) => {
                            app.failed.remove(&id);
                            clusters.insert(id, new_map);
                            app.replace_clusters(clusters.clone());
                        }
                        // Nothing polls any more, so a failed load is not a blip
                        // the next cycle papers over — it is the final word on
                        // that map until someone asks again. Recorded as state
                        // rather than announced as a notice, because a notice
                        // is gone on the next keypress and this is not.
                        MapFetch::Failed => {
                            app.failed.insert(id);
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
                match app.handle_key(key) {
                    Outcome::Quit => return Ok(Ending::Quit),
                    // Through the loaders, not alongside them: a refetch that
                    // raced an in-flight load used to be silently overwritten
                    // by the older snapshot. One channel, send order, newest
                    // write wins. Results stream in as they land, so the last
                    // word on how it went is the count line, not this notice.
                    Outcome::Refresh => {
                        loaders.restart(&app.open_maps, &tx);
                        app.startup.reloading();
                        app.failed.clear();
                    }
                    // Nothing after this can be drawn. Stop the background work
                    // *and wait for it* before handing over: an in-flight `gh`
                    // outlives the `exec` otherwise, and the agent inherits it
                    // as a zombie holding the terminal it just took over.
                    Outcome::Launch(launch) => {
                        loaders.shutdown().await;
                        discovery.abort();
                        let _ = discovery.await;
                        return Ok(Ending::Handover(launch));
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
        args.iter().copied().map(String::from).collect()
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
    fn reap_takes_its_two_flags_in_any_order_or_neither() {
        assert_eq!(
            parse_args(argv(&["reap"])),
            Invocation::Reap {
                yes: false,
                insist: false
            }
        );
        assert_eq!(
            parse_args(argv(&["reap", "-y"])),
            Invocation::Reap {
                yes: true,
                insist: false
            }
        );
        assert_eq!(
            parse_args(argv(&["reap", "-f"])),
            Invocation::Reap {
                yes: false,
                insist: true
            }
        );
        // Both, either way round, long or short: these waive a prompt and a
        // safety guard, so the shapes someone will actually type all have to
        // mean what they look like.
        for both in [
            vec!["reap", "-y", "-f"],
            vec!["reap", "-f", "-y"],
            vec!["reap", "--yes", "--force"],
        ] {
            assert_eq!(
                parse_args(argv(&both)),
                Invocation::Reap {
                    yes: true,
                    insist: true
                },
                "{both:?}"
            );
        }
    }

    #[test]
    fn a_mistyped_reap_flag_is_rejected_rather_than_dropped() {
        // Silently dropping it would leave someone believing they had waived a
        // guard they had not — or, worse, that they had not waived one they had.
        match parse_args(argv(&["reap", "--forse"])) {
            Invocation::Reject(message) => assert!(message.contains("--forse")),
            other => panic!("expected a rejection, got {other:?}"),
        }
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

    #[test]
    fn skills_reports_and_skills_install_links() {
        assert_eq!(parse_args(argv(&["skills"])), Invocation::SkillsReport);
        assert_eq!(
            parse_args(argv(&["skills", "install"])),
            Invocation::SkillsInstall
        );
    }

    #[test]
    fn an_unknown_skills_subcommand_is_rejected_by_name() {
        // `skills` is the one word that takes a second, so its own typos have
        // to be caught here rather than falling into the too-many-arguments
        // arm, which would name the wrong problem.
        match parse_args(argv(&["skills", "instal"])) {
            Invocation::Reject(message) => {
                assert!(message.contains("instal"), "{message}");
                assert!(message.contains("install"), "{message}");
                assert!(!message.contains("too many"), "{message}");
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
        // And a third word is still too many, even after `skills install`.
        match parse_args(argv(&["skills", "install", "now"])) {
            Invocation::Reject(message) => assert!(message.contains("too many"), "{message}"),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn the_usage_names_both_skills_forms() {
        // The help is the only place the subcommand is discoverable, and a
        // flag that exists but is undocumented may as well not.
        assert!(USAGE.contains("wf skills"), "{USAGE}");
        assert!(USAGE.contains("wf skills install"), "{USAGE}");
        assert!(USAGE.contains(skills::BUNDLE_ENV), "{USAGE}");
    }
}
