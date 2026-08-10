//! The picker: the screen, the loop that feeds it, and the handover (#26/#34).
//!
//! A module of the `wf` binary rather than lines of [`main`](crate::main),
//! because of what #137 needs to be able to say. The background reading of what
//! a `wf reap` would claim flows through this assembly — spawned here, carried
//! on the one channel, folded here, drawn from here — and "nothing on that path
//! can delete a workspace" is a claim about *the whole assembly*, not about one
//! expression in it. `main.rs` also owns `wf reap`, which deletes for a living,
//! so a denylist over `main.rs` could never be more than a claim about a region
//! of it. Over this file it is one grep with nothing carved out:
//! [`tests::no_deletion_is_reachable_from_the_picker`].
//!
//! The three joints that grep covers, each of which was reachable before this
//! file existed: [`Picker::tick`]'s drain loop, [`fold`]'s arms, and
//! [`spawn_reading`] — the composition site where the reading becomes a task.
//! Two of them are also watched at run time, as argv, by the probe below.
//!
//! The ordering that matters is at the bottom of [`run_picker`]: restore the
//! terminal, *then* exec, because after the exec there is no `wf` left to
//! restore anything.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::{DefaultTerminal, Terminal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use wf::app::{App, Outcome};
use wf::launch::{Agent, Launch};
use wf::model::{Map, MapId};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{LoadEvent, Loaders, MapFetch, Startup, Survey};
use wf::skills;

/// How long the loop waits on a keypress before redrawing — the cadence at
/// which streamed load events reach the screen.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

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

/// Everything the screen is drawn from, and the arrivals that change it.
///
/// Held together so that [`tick`](Picker::tick) — draining the channel and
/// drawing the result — is a single call a test can make, rather than the body
/// of a loop only a real terminal can enter. That is what lets the probe below
/// drive the *assembly* instead of handing a value straight to [`fold`].
struct Picker {
    app: App,
    clusters: BTreeMap<MapId, Map>,
    loaders: Loaders,
    /// The sending end, kept because a fold can start further loads.
    tx: UnboundedSender<LoadEvent>,
    updates: UnboundedReceiver<LoadEvent>,
}

impl Picker {
    /// A picker over an app that has no data yet (#27), with the cached seed's
    /// fetches already started.
    fn new(
        app: App,
        tx: UnboundedSender<LoadEvent>,
        updates: UnboundedReceiver<LoadEvent>,
    ) -> Self {
        let mut loaders = Loaders::new();
        // The cached seed starts fetching immediately (#28); the search's answer
        // reconciles this set rather than adding to it, so a map that closed or
        // opened is corrected in the tasks actually doing the fetching, not just
        // in the state the screen reads.
        loaders.reconcile(&app.open_maps, &tx);
        Self {
            app,
            clusters: BTreeMap::new(),
            loaders,
            tx,
            updates,
        }
    }

    /// Drain everything that landed, then draw.
    ///
    /// **Deliberately not `async`**, for the reason [`fold`] is not: draining a
    /// channel and painting a buffer read no file, run no process and wait for
    /// nothing. This is the loop body that used to sit inside [`run`], where it
    /// was async by inheritance — and where `reap::remove(id, false).await` was
    /// three lines of edit away from deleting every workspace the reading had
    /// just named, unattended. Here that edit does not compile.
    ///
    /// Generic over the backend so the probe can drive a real tick against a
    /// `TestBackend`: the thing under test is the assembly, and an assembly
    /// only a terminal can enter is one nothing observes.
    fn tick<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        // A fetch swaps one map's cluster; App::replace_clusters keeps the
        // cursor pinned to row identity, query and scope untouched.
        while let Ok(event) = self.updates.try_recv() {
            fold(
                event,
                &mut self.app,
                &mut self.clusters,
                &mut self.loaders,
                &self.tx,
            );
        }
        terminal.draw(|frame| wf::ui::draw(frame, &self.app))?;
        Ok(())
    }
}

/// Fold one arrival into what the screen draws.
///
/// Split out of [`Picker::tick`]'s loop so each arm is a call a test can make
/// and a frame it can read, which is the whole difficulty with an event loop:
/// everything between the channel and the screen used to be reachable only by
/// standing up a terminal and driving it.
///
/// **Deliberately not `async`.** Folding an arrival into the app state reads no
/// file, runs no process and waits for nothing — the work was all done by the
/// task that sent the event. Saying so in the signature is what makes #137's
/// separation structural at this end of the path: this is where a reap would be
/// wired if anyone ever wired one, and a function that cannot await cannot ask
/// `dl` to remove anything.
fn fold(
    event: LoadEvent,
    app: &mut App,
    clusters: &mut BTreeMap<MapId, Map>,
    loaders: &mut Loaders,
    tx: &UnboundedSender<LoadEvent>,
) {
    match event {
        LoadEvent::Discovered(found) => {
            // Reconciling the loaders *is* the load for every map the
            // seed did not already cover: those fetches are all in
            // flight at once and each lands on screen as it arrives.
            loaders.reconcile(&found, tx);
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
                    // `retain` rather than the set method that shares its name
                    // with the thing this whole file may not do — see
                    // [`tests::no_deletion_is_reachable_from_the_picker`]. It
                    // also matches the two lines above it.
                    app.failed.retain(|failed| failed != &id);
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
        // The background reading landed (#137). A plain state write on
        // a screen that has been up and answering keys the whole time —
        // it changes one dim segment of the count line and nothing else,
        // and it arrives at most once because nothing asks again.
        LoadEvent::Reclaimable(found) => {
            app.reclaimable = Some(found);
        }
    }
}

/// Start the background reading of what a `wf reap` would claim (#137).
///
/// The composition site, and a function rather than a line inside
/// [`run_picker`] so that it is somewhere a probe can *call*. Wrapping the
/// reading here — awaiting it before it is handed over, folding a deletion into
/// it — is the edit this covers, and the reason the first frame is measured
/// against it in [`tests::the_first_frame_is_drawn_before_anything_is_asked`].
///
/// Read behind the screen exactly as the map search is: a `dl --ls --json`
/// subprocess and one batched GraphQL call, neither of them on the way to a
/// frame. It folds into the count line when it lands, says nothing when it
/// fails, and can delete nothing.
fn spawn_reading(tx: &UnboundedSender<LoadEvent>) -> Survey {
    wf::refresh::spawn_survey(wf::reclaim::survey_live(), tx.clone())
}

/// Stop everything running behind the screen, and wait for it to be gone.
///
/// The last thing before a handover, and the reason it is a function of its
/// own: "aborted *and awaited*" is a claim about two tasks that a test can now
/// make on both at once. Nothing after this point can be drawn, and the process
/// is about to be replaced — an in-flight `gh` or `dl` that outlives the `exec`
/// is inherited by the agent as a child holding the terminal it just took over.
async fn stop_background(discovery: JoinHandle<()>, survey: Survey) {
    discovery.abort();
    let _ = discovery.await;
    // The reading holds a `dl` and a `gh` of its own, and the same reasoning
    // applies — [`Survey::stop`] is where the waiting is spelt out.
    survey.stop().await;
}

/// The event loop. It starts with **no data at all** (#27): which repos even
/// have maps, and the maps themselves, arrive as [`LoadEvent`]s while the screen
/// is already up and answering keys.
async fn run(
    terminal: &mut DefaultTerminal,
    mut picker: Picker,
    discovery: JoinHandle<()>,
    survey: Survey,
) -> Result<Ending> {
    loop {
        picker.tick(terminal)?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match picker.app.handle_key(key) {
                    Outcome::Quit => return Ok(Ending::Quit),
                    // Through the loaders, not alongside them: a refetch that
                    // raced an in-flight load used to be silently overwritten
                    // by the older snapshot. One channel, send order, newest
                    // write wins. Results stream in as they land, so the last
                    // word on how it went is the count line, not this notice.
                    Outcome::Refresh => {
                        picker.loaders.restart(&picker.app.open_maps, &picker.tx);
                        picker.app.startup.reloading();
                        picker.app.failed.clear();
                    }
                    // Nothing after this can be drawn. Stop the background work
                    // *and wait for it* before handing over: an in-flight `gh`
                    // outlives the `exec` otherwise, and the agent inherits it
                    // as a zombie holding the terminal it just took over.
                    Outcome::Launch(launch) => {
                        picker.loaders.shutdown().await;
                        stop_background(discovery, survey).await;
                        return Ok(Ending::Handover(launch));
                    }
                    Outcome::Continue => {}
                }
            }
        }
    }
}

/// Everything `wf` with no arguments does: register the checkout, put the
/// screen up, run the loop, and either quit or become the agent.
pub async fn run_picker() -> Result<()> {
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

    // The screen goes up *before* any network call (#27). Everything that used
    // to run here — the map search, a serial fetch per repo — now streams into a
    // UI that is already drawn and already reading keys.
    let mut terminal = ratatui::init();
    crate::spawn_terminal_guard();

    let (tx, updates) = mpsc::unbounded_channel();
    let discovery = wf::refresh::spawn_discovery(repos, cache_path.clone(), tx.clone());
    let survey = spawn_reading(&tx);
    let picker = Picker::new(app, tx, updates);
    let ending = run(&mut terminal, picker, discovery, survey).await;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe;

    /// How long the probe lets a spawned task run before it reads the log.
    ///
    /// The reason this is not zero. `#[tokio::test]`'s current-thread runtime
    /// polls a spawned task only at an await point, so a probe body that stops
    /// awaiting after the work it is watching gives `tokio::spawn(async move {
    /// reap::remove(&id, true).await })` a window of exactly nothing — the task
    /// is dropped unpolled and the deletion "did not happen". In the real event
    /// loop that window is seconds. This is the window this probe gives it, and
    /// it is generous on purpose: what is being watched is a `/bin/sh` shim.
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

    /// How long the probe will wait for the background reading to arrive
    /// before giving up and drawing whatever it has. A reading that never lands
    /// leaves the screen without the count line the assertions want, which is a
    /// failure with a readable message rather than a hang.
    const LANDING: std::time::Duration = std::time::Duration::from_secs(10);

    /// A whole `run`-style tick, driven for real in a child process whose `dl`
    /// and `gh` are recording shims.
    ///
    /// This is the assembly, not a value handed to one function of it: the
    /// composition site starts the real [`wf::reclaim::survey_live`], the real
    /// channel carries the real reading, [`Picker::tick`]'s drain loop folds it
    /// and the real [`wf::ui::draw`] paints it. Everything any of that reaches
    /// for is written down as argv, in order, whatever module it was spelt in
    /// and whatever the function was called.
    ///
    /// The runtime is built by hand rather than by `#[tokio::test]` so the
    /// probe can hold it open past the tick and let anything spawned during the
    /// tick actually run — see [`SETTLE`].
    #[test]
    #[ignore = "run by `probe::record` from the tests below, under recording shims"]
    fn picker_tick_probe() {
        if !probe::is_child() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the probe");
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");

        // One `block_on` around the whole thing, so that every tick runs inside
        // the runtime exactly as `run`'s does. A tick driven from outside one
        // would make `tokio::spawn` in a fold arm *panic* rather than run, and
        // a probe that cannot observe the escape it exists for is a probe that
        // reports the wrong reason.
        runtime.block_on(async {
            let (tx, updates) = mpsc::unbounded_channel();
            let survey = spawn_reading(&tx);
            let mut picker = Picker::new(App::empty(), tx, updates);
            // The first frame, with nothing awaited between the spawn above and
            // this line — exactly as `run_picker` has nothing between them. On
            // a current-thread runtime that is what makes the ordering a fact
            // rather than a race: the spawned reading cannot have been polled,
            // so anything in the log above the note was awaited by the frame.
            picker.tick(&mut terminal).expect("the first frame");
            probe::note("the first frame");

            // Now let it land, the way the loop does: tick again each time
            // round, until the reading is on screen.
            let deadline = std::time::Instant::now() + LANDING;
            while picker.app.reclaimable.is_none() && std::time::Instant::now() < deadline {
                tokio::time::sleep(SETTLE / 20).await;
                picker.tick(&mut terminal).expect("a later frame");
            }
            // And give whatever the ticks spawned the window a real loop would.
            tokio::time::sleep(SETTLE).await;
            drop(survey);
        });

        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            println!("{}{}", probe::MARK, line.trim_end());
        }
    }

    /// One recorded run of [`picker_tick_probe`], for the assertions below to
    /// share. Each of them calls this rather than the probe directly, because
    /// the child costs a process and a `SETTLE`.
    fn a_tick() -> probe::Recording {
        probe::record(
            "picker::tests::picker_tick_probe",
            probe::DL_LISTING,
            probe::GH_FACTS,
        )
    }

    #[test]
    fn no_deletion_survives_a_whole_tick_of_the_real_assembly() {
        // #137's safety claim as a fact about a run, over the assembly rather
        // than over one expression in it. Between the composition site and the
        // painted frame there is exactly one thing this may ask of the machine:
        // what exists, and what the tracker says about it. A `dl <ws> rm` added
        // anywhere in that span — in the drain loop, inside a `tokio::spawn`,
        // behind a helper named nothing anyone thought to forbid — is recorded
        // here, because destroying a workspace means running `dl`.
        let run = a_tick();
        run.destroyed_nothing();
        assert_eq!(
            run.argv.len(),
            3,
            "a tick asks for the listing and the tracker, and nothing else: {:?}",
            run.argv
        );
        assert_eq!(run.argv[1], "dl <--ls> <--json>");
        assert!(
            run.argv[2].starts_with("gh <api> <graphql> <-F> <owner=blooop> <-F> <name=wayfinder>"),
            "one batched read of the tracker: {}",
            run.argv[2]
        );
    }

    #[test]
    fn the_first_frame_is_drawn_before_anything_is_asked() {
        // #137's other property, pinned rather than asserted. The reading costs
        // a subprocess and a round trip; the first frame must not be behind
        // either. The probe writes its note the instant the frame is painted,
        // into the same log the shims append to, so the ordering is a recorded
        // fact: awaiting the reading at the composition site — which *does*
        // compile — puts `dl <--ls> <--json>` above the note and fails here.
        let run = a_tick();
        assert_eq!(
            run.argv.first().map(String::as_str),
            Some("note <the first frame>"),
            "the first frame waited for something: {:?}",
            run.argv
        );
    }

    #[test]
    fn a_landed_reading_reaches_the_screen_through_the_loop() {
        // The wiring #137 is, end to end at this end of it: the reading is
        // taken behind the screen, arrives on the channel, is drained by a
        // tick, and the count line says what a reap would claim. An arm that
        // dropped the payload on the floor — `Reclaimable(_) => {}` — leaves
        // every other test in this tree green, because every other test hands
        // the reading to the thing it is testing.
        let run = a_tick();
        let screen = run.printed();
        let line = screen
            .iter()
            .find(|line| line.contains("reclaimable"))
            .unwrap_or_else(|| panic!("the count line must say so:\n{}", screen.join("\n")));
        assert!(line.contains("1 reclaimable"), "{line}");
        assert!(
            line.contains("wf-129-closed"),
            "it names the workspace: {line}"
        );
        assert!(line.contains("wf reap"), "and the command: {line}");
    }

    #[test]
    fn no_deletion_is_reachable_from_the_picker() {
        // The structural half, which the run above cannot cover: a subprocess
        // is observable and `std::fs::remove_dir_all` is not. So argv is
        // watched at run time and the means of destruction that leave no argv
        // are pinned here, over the *whole* file — the composition site, the
        // drain loop, every arm of the fold and the loop around them. This
        // module drains a channel, writes app state and paints a buffer, and
        // has no business naming any of these.
        //
        // `reap` is on the list bare, not as `reap::`, and that is the point:
        // `use wf::reap as tidy;` spells no `::` and would then reach the
        // deletion side under a name nobody forbade. Every route to that module
        // has to say the word somewhere, so the word is what is forbidden.
        // `tokio::spawn` is on it because a spawned task is how an `.await`
        // gets into a function that has none, which is the one way round
        // [`Picker::tick`] and [`fold`] not being async.
        let code = probe::code_only(include_str!("picker.rs"));
        for forbidden in [
            "reap",
            "remove",
            "\"rm\"",
            "--force",
            "Command",
            "process::",
            "fs::",
            "unsafe",
            "tokio::spawn",
        ] {
            assert!(
                !code.contains(forbidden),
                "the picker must not be able to delete a workspace: it names {forbidden:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_handover_leaves_no_background_work_running() {
        // The last thing before `exec`, and the one that cannot be seen from
        // the outside afterwards: both tasks must be aborted *and waited for*,
        // because it is dropping their futures that closes the `gh` and `dl`
        // they hold. An `abort()` without the wait, a handle dropped, a handle
        // forgotten — all three leave a child alive for the agent to inherit.
        //
        // Each task holds an `Arc` that cannot be released until its future is
        // dropped, so the counts are the witness.
        let (tx, _rx) = mpsc::unbounded_channel();
        let witness = std::sync::Arc::new(());
        let for_discovery = std::sync::Arc::clone(&witness);
        let for_survey = std::sync::Arc::clone(&witness);
        let discovery = tokio::spawn(async move {
            std::future::pending::<()>().await;
            drop(for_discovery);
        });
        let survey = wf::refresh::spawn_survey(
            async move {
                std::future::pending::<()>().await;
                drop(for_survey);
                None
            },
            tx,
        );
        stop_background(discovery, survey).await;
        assert_eq!(
            std::sync::Arc::strong_count(&witness),
            1,
            "background work outlived the handover"
        );
    }
}
