//! The picker: the screen, the loop that feeds it, and the handover (#26/#34).
//!
//! The background reading of what a `wf reap` would claim flows through this
//! assembly — spawned in [`session`], carried on the one channel, folded in
//! [`fold`], drawn from [`Picker::tick`] — and #137 asks that nothing on that
//! path delete a workspace. Four rounds of review each found a different hole
//! in a stronger version of this note than the one below, so it is worth being
//! exact about what each guard covers and where each one stops.
//!
//! 1. **The deletion itself cannot be *named*.** `wf::reap`'s `remove` is
//!    private to its module, in the library; this is the binary. A prologue in
//!    `main`, a helper in `main.rs`, an aliased import, a submodule of this
//!    file — every route that ends at that function is a compile error, and
//!    nothing has to notice, because the build refuses.
//!
//!    That does **not** close `wf::reap::run`, which is public because `main`
//!    must dispatch `wf reap`, and which contains the deletion:
//!    `reap::run(true, true)` *is* the forced reap. Visibility cannot separate
//!    the picker from a function the same binary has to call, so what stands in
//!    its place is a denylist — `reap` is a forbidden token in this file
//!    ([`tests::the_picker_names_neither_a_subprocess_nor_reap`]) and `main.rs`
//!    holds the list of every line of its own *code* that writes the word. Not
//!    every line: the `USAGE` help text writes `wf reap` twice, because that is
//!    the command it documents, and it is cut out before the list is taken —
//!    with the cut checked against `USAGE`'s own line count, because an
//!    unbounded cut was itself an escape. Those are
//!    greps over the source text of two files: they catch a cleanup wired in by
//!    accident, and they do not stop anyone who means to get around them. The
//!    same call made one hop away in a third file is caught by neither, and so
//!    is a second name for the module re-exported from the library. Those holes
//!    are real; the only thing that would close them is the picker living in a
//!    crate that cannot depend on reap at all, which is a change to `wf`'s
//!    build and its own decision (#137).
//! 2. **Running a command is watched, across every arm of the loop.**
//!    [`tests::no_deletion_survives_a_whole_session_of_the_real_assembly`]
//!    drives [`session`] for real against recording `dl` and `gh` shims, so a
//!    `dl <ws> rm` reached from anywhere the loop can reach — a helper in
//!    `app.rs`, a task spawned in a fold arm, a function named nothing anyone
//!    thought to forbid — is written down as argv. It does not care what file
//!    the route was spelt in. It cares about *where*, about *which arm*, and
//!    about *when*, and each of those three is a bound on the claim rather than
//!    a detail of the test:
//!
//!    - **where**: the recording starts at [`session`], not at [`run_picker`].
//!      `run_picker` composes the registration, the terminal, the `session`
//!      call and the handover, and no test in this repository executes it, so
//!      everything it does above and below that call is outside the recording.
//!      What stands there instead is the denylist in (1), which is why it
//!      carries `fs` — twice over: a reviewer deleted a workspace from
//!      `run_picker` with `std::fs::remove_dir_all` and the whole selection
//!      green, and when the token was added as `fs::`, the next reviewer
//!      reopened it with `use std::fs as sys;`. It is the bare name now, for
//!      the reason `reap` is;
//!    - **which arm**: all three [`Outcome`] arms are driven, the launch one
//!      included, and so is [`fold`] — but only through the events the child's
//!      fixtures actually produce. Its `Discovered`, `Fetched` and
//!      `Surveyed` arms are entered; `SearchFailed` is not, because the `gh`
//!      shim answers the map search successfully, and a `dl <ws> rm` planted in
//!      that one arm is green. What stands against it there is the denylist in
//!      (1) — it has to be spelt in another file to get past that; the
//!      recording does not reach it;
//!    - **when**: to a stated window past the ending (`WATCH`, in the tests
//!      below). A cleanup deferred longer than that is not seen. The bound is
//!      stated rather than silent because it cannot be removed — a probe can
//!      always be outwaited.
//! 3. **Destruction that runs no command is watched where it would land.** The
//!    same run gives the child a scratch `HOME` laid out the way this machine
//!    is — `~/.cache/devlaunch/repos/<owner>/<repo>/<id>` for the clone,
//!    `~/.devpod/contexts/<ctx>/workspaces/<id>` for the record — and compares
//!    the tree before and after. A `remove_dir_all` aimed at a workspace fails
//!    it *if it runs inside `session`* — the same three bounds as (2) apply,
//!    and the `run_picker` escape above was one of them being real. An `fs`
//!    call aimed outside that home, **or written outside `session`** — which is
//!    the whole of [`run_picker`] and the whole of `main.rs` — is caught by
//!    nothing here; what covers those two files is the denylist in (1), and
//!    nothing covers a third file. That is as true of any code in any crate,
//!    and this file claims no better.
//!
//! None of that says the picker deletes nothing at all. [`run_picker`] calls
//! [`refresh_skills`] on the way into the agent, and a copy that has fallen
//! behind the bundle is rewritten by removing it first — `wf::skills`' `clear`
//! is a `remove_dir_all` under `~/.claude/wf-skills`, reached from this file's
//! own code on this file's own launch path. It is `wf`'s copy of `wf`'s own
//! prompts, not a workspace and not anyone's work; the probe above stops one
//! line short of it; and the claim here is about workspaces, which is why it is
//! written as one rather than as "nothing here deletes".
//!
//! The ordering that matters is at the bottom of [`run_picker`]: restore the
//! terminal, *then* exec, because after the exec there is no `wf` left to
//! restore anything.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use ratatui::Terminal;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use wf::app::{App, Outcome};
use wf::launch::{Agent, Handoff, Launch};
use wf::model::{Map, MapId};
use wf::projects::{self, ProjectsCache};
use wf::refresh::{LoadEvent, Loaders, MapFetch, Startup, Survey};
use wf::skills;

/// How long the loop waits on a keypress before redrawing — the cadence at
/// which streamed load events reach the screen.
const TICK: Duration = Duration::from_millis(250);

/// Where the loop's keypresses come from.
///
/// Injected rather than read straight from the terminal, and that is the
/// difference between [`run`] being observable and not. A loop that calls
/// `crossterm::event::poll` can only be entered by a process attached to a
/// tty — so for three review rounds `run`'s body was the one part of this
/// assembly nothing drove, and every escape that was found in that time was
/// written into it. The probe below now drives the real loop with a scripted
/// keyboard; anything the loop reaches for is recorded as argv.
///
/// `async` because the loop has no other await point. On the real terminal
/// this blocks the thread inside `poll`, exactly as the inline call it
/// replaced did; in the probe it is a `sleep`, which is what lets a
/// current-thread runtime poll the background reading between frames — and
/// what gives a task a fold arm *spawned* the window a real loop would.
trait Keys {
    /// The next key **press**, or `None` when `timeout` elapsed with nothing
    /// to report. Releases and repeats are not presses and are not reported.
    ///
    /// `app` is what was just painted. The real keyboard has no use for it —
    /// the person reading the screen is the one holding that — but it is what
    /// lets the probe drive the loop the way a person does: watch for the
    /// thing it came to see, then quit.
    async fn next_press(&mut self, app: &App, timeout: Duration) -> Result<Option<KeyEvent>>;
}

/// The real keyboard: whatever is typed at the terminal `wf` is attached to.
struct Typed;

impl Keys for Typed {
    // No `.await` here, and there cannot be one: `poll` and `read` are
    // crossterm's blocking calls, and blocking the thread inside them *is*
    // this impl's behaviour (see the trait's own note above). The `async` is
    // the trait's, not this function's — dropping it would mean dropping it
    // from the trait, which the probe's impl needs to sleep. Clippy 1.98 reads
    // an await-free async trait impl as an accident; here it is the
    // arrangement being described.
    //
    // `unknown_lints` rides along because the lint below does not exist before
    // 1.98, and `recipe/recipe.yaml` builds on an older toolchain than CI's
    // stable: without it, naming the lint fails the build everywhere it is not
    // yet known. Both attributes go together or neither works.
    #[allow(unknown_lints)]
    #[allow(clippy::unused_async_trait_impl)]
    async fn next_press(&mut self, _app: &App, timeout: Duration) -> Result<Option<KeyEvent>> {
        if !event::poll(timeout)? {
            return Ok(None);
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(key)),
            // A release, a repeat, a resize, a paste: nothing the picker acts
            // on, and indistinguishable from the timeout as far as the loop is
            // concerned — it redraws and asks again.
            _ => Ok(None),
        }
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
    /// A ticket was picked. `wf`'s last act is to become its agent, carrying
    /// the stamps of what `wf` itself did on the way here (#160).
    Handover(Box<Launch>, Handoff),
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
    /// nothing, and the signature is where that is said.
    ///
    /// Generic over the backend so the probe can drive a real tick against a
    /// `TestBackend` — the same genericity [`run`] has, and for the same
    /// reason: an assembly only a terminal can enter is one nothing observes.
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
/// task that sent the event, and a signature that says so is a signature the
/// next reader can rely on.
///
/// It is worth saying what that does *not* buy, because an earlier round of
/// this file said it did. Not being async rules out `.await` in these arms and
/// nothing more: a synchronous `std::process::Command`, or a `tokio::spawn`ed
/// task, reaches `dl` from here perfectly well. What stands against a deletion
/// in this arm is the module doc's (2) and (3) — a command it runs is recorded,
/// and a workspace it removes in-process leaves a hole in the scratch home —
/// with the limits stated there.
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
        // The background reading landed (#137). A plain state write on
        // a screen that has been up and answering keys the whole time.
        // It reaches further than the count line's dim segment: through
        // `app.liveness` it marks any row whose node this machine has
        // something to say about.
        //
        // It arrives exactly once. This arm used to be two — a guard
        // dropping answers to a question the refresh key had withdrawn, and
        // the fold proper — because the reading is silent when it finds
        // nothing, so a stale answer folded after a clear would restore it
        // with nothing able to correct it. With no key to clear anything
        // there is one question and one answer, and one arm.
        LoadEvent::Surveyed(reading) => {
            let (reclaimable, liveness) = reading.into_parts();
            app.reclaimable = reclaimable;
            app.liveness = liveness;
        }
    }
}

/// Start the background reading of what a `wf reap` would claim (#137).
///
/// A function rather than a line inside [`session`] because *spawning* the
/// reading — as against awaiting it — is the property
/// [`tests::the_first_frame_is_drawn_before_anything_is_asked`] pins, and a
/// name is somewhere to say that. Its only caller is [`session`], which the
/// probe drives whole, so this is not a seam anything reaches around.
///
/// Read behind the screen exactly as the map search is: a `dl --ls --json`
/// subprocess and one batched GraphQL call, neither of them on the way to a
/// frame. What lands folds into the count line and into the rows' own
/// markings; a reading that fails says nothing at all.
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
///
/// Generic over the backend and over [`Keys`] so that the probe drives *this*
/// function rather than a hand-written imitation of it, and it drives every one
/// of the three arms below: a scripted keyboard that only ever sent `Esc` left
/// `Launch` unentered, which is where a fifth escape was found. Whatever the
/// loop body reaches — a helper in another module, a task it spawns, a `dl` it
/// shells out to — is recorded there as argv, for as long as that probe
/// watches.
///
/// There were four arms until the refresh key was retired, and the fourth was
/// the only one that ever *wrote* to the picker instead of reading it: it
/// restarted the loads, put the startup state back into loading, cleared the
/// failures, threw the reading away and took another. What is left only drains,
/// draws and ends — which is why nothing here holds a generation, and why
/// `survey` is handed straight on rather than being something this function can
/// replace.
async fn run<B: Backend, K: Keys>(
    terminal: &mut Terminal<B>,
    keys: &mut K,
    mut picker: Picker,
    discovery: JoinHandle<()>,
    survey: Survey,
) -> Result<Ending> {
    loop {
        picker.tick(terminal)?;
        let Some(key) = keys.next_press(&picker.app, TICK).await? else {
            continue;
        };
        match picker.app.handle_key(key) {
            Outcome::Quit => return Ok(Ending::Quit),
            // Nothing after this can be drawn. Stop the background work
            // *and wait for it* before handing over: an in-flight `gh`
            // outlives the `exec` otherwise, and the agent inherits it
            // as a zombie holding the terminal it just took over.
            Outcome::Launch(launch) => {
                // **The keystroke is stamped here, first, before anything is
                // waited on** (#160). This is the instant a human's part in
                // the launch ends: `handle_key` has just turned a keypress
                // into a launch, and everything from this line to the agent's
                // first frame is machine time — the shutdowns below, the cache
                // write, the exec, and `dl`'s own start. Taking it any later
                // would quietly drop `wf`'s own hand-over cost out of the one
                // measurement that spans the exec; taking it any earlier would
                // fold in the human, who was still choosing a mode.
                let handoff = Handoff::now(picker.app.prewarm_fired(&launch));
                picker.loaders.shutdown().await;
                stop_background(discovery, survey).await;
                return Ok(Ending::Handover(launch, handoff));
            }
            Outcome::Continue => {}
        }
    }
}

/// Compose the picker over one channel and run it to an [`Ending`].
///
/// Everything between the terminal and the ending: the two pieces of background
/// work, the state the screen is drawn from, and the loop that joins them. A
/// function rather than lines of [`run_picker`] because this is the composition
/// site — where the reading becomes a task — and the probe drives it whole.
/// Awaiting the reading here rather than spawning it, folding a deletion into
/// the loop, reaching for `dl` from a helper in any other file: all of it is
/// inside what the probe records.
async fn session<B: Backend, K: Keys>(
    terminal: &mut Terminal<B>,
    keys: &mut K,
    app: App,
    repos: Vec<String>,
    cache_path: PathBuf,
) -> Result<Ending> {
    let (tx, updates) = mpsc::unbounded_channel();
    let discovery = wf::refresh::spawn_discovery(repos, cache_path, tx.clone());
    let survey = spawn_reading(&tx);
    let picker = Picker::new(app, tx, updates);
    run(terminal, keys, picker, discovery, survey).await
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
    let (cache, saved) = ProjectsCache::at_startup(
        &cache_path,
        here.as_ref()
            .map(|(path, slug)| (path.as_path(), slug.as_str())),
    );
    // Reported, not fatal — the launcher runs on the registry it just loaded
    // whether or not the machine can remember it, exactly as the handover write
    // does. Printed *here*, before the alternate screen goes up, so it lands on
    // the primary buffer: visible on the way in, and still there when `wf` puts
    // that buffer back on the way out.
    if let Err(err) = saved {
        eprintln!("wf: could not save the projects cache: {err:#}");
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

    // Everything from here to the ending is [`session`], which the probe drives
    // for real against a `TestBackend` and a scripted keyboard. What is left
    // outside it is this terminal and the handover below, neither of which a
    // test can stand up.
    let ending = session(&mut terminal, &mut Typed, app, repos, cache_path.clone()).await;

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
        Ending::Handover(launch, handoff) => {
            // The launch is a use of this project, and the last chance to say
            // so: after the exec there is no `wf` left to write anything. This
            // is what keeps the project list ordered for someone who reaches
            // their projects *through* it — opening `wf` in a checkout stamps
            // it, and for everyone else launching is the only other act that
            // means "this is what I am working on".
            //
            // Through the cache's write seam, which re-reads: the discovery
            // task writes the search's findings to this same file while the
            // picker is up, so the copy loaded before the first frame is stale
            // by now, and writing it back would trade this stamp for next run's
            // head start.
            //
            // Best-effort on purpose. A cache that will not write is not worth
            // refusing a launch over, and the only cost of losing this is one
            // project sitting lower in a list than it might have.
            //
            // The session record rides along in the same edit — the
            // conversation this launch is about to start, so a later run can
            // offer the way back into it (#35).
            //
            // Written **here**, immediately before the terminal is restored
            // and the image replaced, because this is the last moment `wf`
            // exists — and written from the resolved launch rather than from
            // the picker, so what is remembered is the tree the agent actually
            // gets. A creation records nothing: it has no node to key on until
            // its skill files one.
            //
            // Best-effort like the stamp it travels with, and for a smaller
            // cost: a record that fails to write means the resume row is
            // missing next time, not that anything is wrong with the launch.
            let _ = ProjectsCache::update(&cache_path, |cache| {
                let mut changed = cache.touch(launch.cwd());
                if let Some(session) = launch.session() {
                    cache.record_session(session);
                    changed = true;
                }
                changed
            });
            // The prompts the selected agent is about to run. `wf skills
            // install` links its skills directory at a *copy* of the bundle,
            // and a copy is a thing that can fall behind a `pixi global update
            // wf`. This is where it cannot: the process that refreshes it is
            // the same one that then execs the prompt, so no launch ever gets
            // ahead by even one release.
            refresh_skills(launch.agent());
            // Only ever returns an error: on success this process *is* the agent.
            Err(launch.exec(&handoff))
        }
    }
}

/// Bring the installed skills back in step with the bundle they were installed
/// from — their contents, and which of them this machine has links for at all.
///
/// Best-effort, and deliberately silent when there is nothing to do: a machine
/// with no home directory to resolve, and one that never ran
/// `wf skills install`, are not worth a word on the way into an agent. Two
/// things are worth one. A copy that could not be *written*, because the agent
/// is then about to run a prompt that is not the one that was installed. And a
/// link that had to be *created*, because that means this launch changed which
/// prompts this machine has — the thing that used to happen only at an install
/// somebody typed, and that a release's worth of launches did silently not do
/// (#170).
fn refresh_skills(agent: Agent) {
    let Ok(target) = skills::Target::resolve(agent) else {
        return;
    };
    match skills::refresh(&target) {
        Err(err) => eprintln!(
            "wf: could not refresh the {} skills: {err:#}",
            agent.label()
        ),
        Ok(healed) => {
            for (name, what) in healed {
                match what {
                    // The errand every launch runs; saying so would be noise.
                    skills::Healed::Copied => {}
                    skills::Healed::Linked { was: None } => eprintln!(
                        "wf: linked the {name} skill for {}, which this build \
                         ships and this machine had no link for",
                        agent.label()
                    ),
                    skills::Healed::Linked { was: Some(old) } => eprintln!(
                        "wf: repointed the {name} skill for {}, which still linked to {}",
                        agent.label(),
                        old.display()
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    /// How long the probe waits after each thing it does — after the reading
    /// reaches the screen, after each key it types, and after the session ends.
    ///
    /// The reason this is not zero. A current-thread runtime polls a spawned
    /// task only at an await point, so a probe that stopped the moment it had
    /// what it came for would give `tokio::spawn(async move { … })` in a fold
    /// arm a window of exactly nothing — the task is dropped unpolled and the
    /// deletion "did not happen". In the real event loop that window is
    /// seconds.
    ///
    /// **This is the bound on what the recording claims, and it is stated
    /// rather than hidden**, because it cannot be removed. A cleanup that
    /// sleeps for longer than the probe watches is not observed, and no choice
    /// of number changes that — a longer window costs suite time and buys a
    /// bigger number to sleep past. What is bought instead is that the window
    /// is *everywhere* rather than only at the end: the probe waits this long
    /// between every key, so a task spawned in any arm is polled before the
    /// next thing happens, and again after the session returns.
    pub const WATCH: Duration = Duration::from_millis(400);

    /// How long the probe will wait for the background reading to arrive
    /// before quitting anyway. A reading that never lands leaves the screen
    /// without the count line the assertions want, which is a failure with a
    /// readable message rather than a hang.
    const LANDING: Duration = Duration::from_secs(10);

    /// A scripted keyboard, and the only thing about the probe's run that is
    /// not the real thing.
    ///
    /// It drives the loop the way a person does: watch the screen, wait for
    /// what you came for, then type. The first time it is asked for a key the
    /// first frame has just been painted and nothing else has happened yet, so
    /// that is where the ordering note goes.
    ///
    /// The keys it types are a *plan*, because one of them is not enough. `run`
    /// has three [`Outcome`] arms and for three review rounds this script sent
    /// only `Esc`, so `Launch` was never entered and a deletion written into it
    /// was green — and `Launch` is both the product's primary action and the
    /// natural home of a "free the disk on the way out" cleanup. A plan that
    /// runs out falls back to `Esc`, so a loop that failed to end where it was
    /// supposed to ends anyway and fails on an assertion rather than on the
    /// suite's timeout.
    struct Script {
        /// What to type, in order, one key per [`WATCH`].
        plan: std::collections::VecDeque<KeyEvent>,
        /// When to start typing regardless, so a reading that never lands does
        /// not hang the run.
        deadline: std::time::Instant,
        /// When the next key may be sent: [`WATCH`] after the reading landed,
        /// and [`WATCH`] after each key since. `None` until the reading lands.
        due: Option<std::time::Instant>,
        /// Whether the first frame has been noted yet.
        noted: bool,
    }

    impl Script {
        fn new(plan: Vec<KeyEvent>) -> Self {
            Self {
                plan: plan.into(),
                deadline: std::time::Instant::now() + LANDING,
                due: None,
                noted: false,
            }
        }
    }

    /// A key with no modifiers, which is all this script needs. There was a
    /// `ctrl` variant beside it for the one chord that reached an [`Outcome`]
    /// of its own — refresh, now retired. The chords that are left either quit
    /// (`ctrl-c`, which `Esc` already covers here) or move the cursor, and the
    /// plain keys reach both.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    impl Keys for Script {
        async fn next_press(&mut self, app: &App, timeout: Duration) -> Result<Option<KeyEvent>> {
            let now = std::time::Instant::now();
            if !self.noted {
                self.noted = true;
                // The first frame is painted and nothing has been awaited
                // since: on a current-thread runtime the reading spawned at the
                // composition site cannot have been polled yet, so anything
                // already in the log above this note was waited for by the
                // frame. That is the whole of what
                // `the_first_frame_is_drawn_before_anything_is_asked` reads.
                probe::note("the first frame");
            }
            if self.due.is_none() && (app.reclaimable.is_some() || !app.liveness.is_empty()) {
                self.due = Some(now + WATCH);
            }
            let ready = self.due.is_some_and(|at| now >= at);
            if ready || now >= self.deadline {
                self.due = Some(now + WATCH);
                return Ok(Some(self.plan.pop_front().unwrap_or(press(KeyCode::Esc))));
            }
            // Far shorter than the real `TICK`, because this is the probe's own
            // cadence and a probe that took a quarter of a second per frame
            // would cost seconds per assertion. It is an `await`, which is what
            // matters: it is the only point in the loop at which anything
            // spawned gets to run.
            tokio::time::sleep(timeout / 25).await;
            Ok(None)
        }
    }

    /// A whole session, driven for real in a child process whose `dl` and `gh`
    /// are recording shims and whose `HOME` is a scratch machine laid out the
    /// way this one is, with the fixture's workspaces on it.
    ///
    /// This is the assembly and not an imitation of it: [`session`] is the
    /// function `run_picker` calls, it starts the real
    /// [`wf::reclaim::survey_live`] and the real discovery, the real channel
    /// carries the real reading, [`run`]'s own loop turns, [`Picker::tick`]
    /// drains and the real [`wf::ui::draw`] paints. Everything any of that
    /// reaches for is written down — as argv if it ran a command, as a
    /// disturbed path if it touched the scratch home — whatever module it was
    /// spelt in, under whatever name, from whatever submodule.
    ///
    /// The runtime is built by hand rather than by `#[tokio::test]` so that it
    /// is current-thread: that is what makes the ordering above deterministic
    /// rather than a race between the first frame and a worker thread. It is
    /// also kept alive past the session and driven for one more [`WATCH`], so
    /// that a task spawned by the arm that *ended* the loop is polled rather
    /// than dropped with the runtime — the launch arm being exactly where a
    /// cleanup on the way out would go.
    ///
    /// The screen is printed on the way out for the assertions that read it.
    fn drive(app: App, plan: Vec<KeyEvent>) -> Ending {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the probe");
        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut keys = Script::new(plan);

        // No repos, so the map search answers immediately without asking `gh`
        // anything — the reading is what this run is about, and a discovery
        // that fetched would only add noise to the log. The cache it writes
        // goes in the probe's own scratch directory rather than under `HOME`,
        // because `HOME` is the thing being watched for changes.
        let ending = runtime.block_on(session(
            &mut terminal,
            &mut keys,
            app,
            Vec::new(),
            probe::child_scratch().join("projects.json"),
        ));
        // Built inside the runtime's context, not passed into it: a `sleep`
        // constructed outside one has no timer to register with.
        runtime.block_on(async { tokio::time::sleep(WATCH).await });

        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            println!("{}{}", probe::MARK, line.trim_end());
        }
        ending.expect("the session")
    }

    /// The quit path, with the one arm that does not end the loop driven on the
    /// way: `down` on a screen with nothing on it is [`Outcome::Continue`],
    /// `esc` is [`Outcome::Quit`].
    #[test]
    #[ignore = "run by `probe::record` from the tests below, under recording shims"]
    fn picker_session_probe() {
        if !probe::is_child() {
            return;
        }
        let ending = drive(
            App::empty(),
            vec![press(KeyCode::Down), press(KeyCode::Esc)],
        );
        assert!(
            matches!(ending, Ending::Quit),
            "the scripted keyboard quits, and nothing else ends this loop"
        );
    }

    /// The launch path — [`Outcome::Launch`], the fourth arm and the product's
    /// primary action, which no probe drove for the first four rounds of this
    /// PR.
    ///
    /// A registered checkout with no map on it is the shortest real route to
    /// one: the project's own row is the first stop, `enter` opens the launch
    /// picker on its creation candidates, `down` moves to `new map` — which
    /// needs no text typed into it — and `enter` resolves it against the one
    /// registered checkout and returns a launch. The loop then shuts the
    /// background work down and returns [`Ending::Handover`], which is where
    /// `run_picker` would `exec`; this stops one line short of that.
    ///
    /// The checkout path is in the probe's own scratch directory rather than
    /// under `HOME`, because `HOME` is what is being watched for changes.
    ///
    /// The checkout carries a `devcontainer.json`, so the launch this drives is
    /// an **isolated** one — `dl`, not `claude`. That is deliberate and it is
    /// the more interesting of the two arms: isolation is conditional on a
    /// subprocess answer (`dl --version`, against `DEVLAUNCH_FLOOR`), and a
    /// condition met by asking another program is the kind that degrades
    /// silently. A `wf` that misread the answer would go on launching perfectly
    /// well — on the host, with no container and no per-node branch, which is a
    /// different product. Nothing in the library could see that: the probe is
    /// where the question is actually asked.
    ///
    /// Paired with [`picker_old_dl_launch_probe`], which drives the same keys
    /// against a `dl` below the floor. Neither is worth much alone — this one
    /// says a new `dl` gets the launch, that one says an old `dl` does not, and
    /// only the two together say the *floor* is what decides. See
    /// [`the_version_floor_is_what_decides_whether_a_launch_is_isolated`].
    #[test]
    #[ignore = "run by `probe::record_as_dl` from the tests below, under recording shims"]
    fn picker_launch_probe() {
        if !probe::is_child() {
            return;
        }
        let (checkout, ending) = drive_a_launch_from_a_devcontainer_checkout();
        match ending {
            Ending::Handover(launch, handoff) => {
                assert_eq!(
                    launch.cwd(),
                    checkout,
                    "the launch resolves to the one registered checkout"
                );
                assert_eq!(
                    launch.isolation(),
                    wf::launch::Isolation::Devlaunch,
                    "a devcontainer and a `dl` above the floor is an isolated launch"
                );
                // And it carries the keystroke it came from (#160). Taken from
                // the run rather than from a fixture: the claim is that the
                // loop stamps the launch arm at all, which is a fact about
                // this assembly and about no value a test could hand it.
                let [(_, t0), (_, prewarm_fired)] = launch.stamps(&handoff);
                let stamped: f64 = t0
                    .expect("a handover to `dl` stamps its keystroke")
                    .parse()
                    .expect("the stamp is epoch seconds");
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .expect("this machine's clock is past 1970")
                    .as_secs_f64();
                assert!(
                    (0.0..60.0).contains(&(now - stamped)),
                    "the stamp is this keystroke's instant, not some other run's: \
                     {stamped} against {now}"
                );
                // Nothing was warmed — the prewarm is opt-in and this run did
                // not ask — so the second variable says nothing rather than
                // saying zero.
                assert_eq!(prewarm_fired, None);
                // The enum and not also the argv. An earlier draft asserted
                // `agent_argv()[0] == "dl"` beside this, on the theory that a
                // decision and what reaches `execvp` are different things —
                // but `agent_argv` is a `match self.isolation` and nothing
                // else, so the two cannot disagree and the second assertion
                // was the first one restated. What the argv actually carries
                // — the quoting, the workspace, the `--` — is pinned where it
                // meets a real shell, in tests/live_launch_exec.rs.
            }
            Ending::Quit => {
                panic!("the launch arm was never entered — the plan ran out and the fallback quit")
            }
        }
    }

    /// The same launch, against a `dl` too old for the floor: it must come back
    /// to the host rather than be handed to a `dl` that cannot do what `wf`
    /// would ask of it.
    ///
    /// This is the half that makes the conditional observable. Deleting
    /// `devlaunch_on_path() == Devlaunch::Usable` from `Isolation::detect`
    /// leaves every other assertion in this module green — the version is still
    /// asked, because the startup listing asks it and the answer is memoized —
    /// and fails here, which is the only place the two versions are told apart.
    #[test]
    #[ignore = "run by `probe::record_as_dl` from the tests below, under recording shims"]
    fn picker_old_dl_launch_probe() {
        if !probe::is_child() {
            return;
        }
        let (_, ending) = drive_a_launch_from_a_devcontainer_checkout();
        match ending {
            Ending::Handover(launch, handoff) => {
                assert_eq!(
                    launch.isolation(),
                    wf::launch::Isolation::Host,
                    "a `dl` below the floor is a `dl` `wf` will not launch into"
                );
                // And the seam is empty with it (#160): the stamps are for the
                // `dl` this launch is no longer going to become, and one left
                // in the agent's environment would be read by whatever `dl`
                // that session ran next.
                assert!(
                    launch
                        .stamps(&handoff)
                        .iter()
                        .all(|(_, stamp)| stamp.is_none()),
                    "a host launch hands the seam nothing: {:?}",
                    launch.stamps(&handoff)
                );
            }
            Ending::Quit => {
                panic!("the launch arm was never entered — the plan ran out and the fallback quit")
            }
        }
    }

    /// The setup both launch probes share: one registered checkout carrying a
    /// `devcontainer.json`, and the three keys that reach a launch from it.
    ///
    /// Shared so the two differ in exactly one thing — the version their `dl`
    /// answers with — because that is the whole claim they make together. The
    /// checkout is in the probe's own scratch directory rather than under
    /// `HOME`, because `HOME` is what is being watched for changes.
    fn drive_a_launch_from_a_devcontainer_checkout() -> (PathBuf, Ending) {
        let repo = "blooop/wayfinder";
        let checkout = probe::child_scratch().join("checkout");
        std::fs::create_dir_all(checkout.join(".devcontainer")).expect("the scratch checkout");
        std::fs::write(checkout.join(".devcontainer/devcontainer.json"), "{}\n")
            .expect("the checkout's devcontainer");
        let mut app = App::empty().with_checkouts(vec![projects::Checkout::new(
            checkout.clone(),
            repo.to_string(),
        )]);
        app.enter(repo);
        let ending = drive(
            app,
            vec![
                press(KeyCode::Enter),
                press(KeyCode::Down),
                press(KeyCode::Enter),
            ],
        );
        (checkout, ending)
    }

    /// One recorded run of [`picker_session_probe`], for the assertions below
    /// to share. Each of them calls this rather than the probe directly,
    /// because the child costs a process and several [`WATCH`]es.
    fn a_session() -> probe::Recording {
        probe::record(
            "picker::tests::picker_session_probe",
            probe::DL_LISTING,
            probe::GH_FACTS,
        )
    }

    /// Is this recorded call one of the questions a session is allowed to ask?
    ///
    /// Shared by the two safety assertions so they cannot drift apart, and
    /// written as "what may be asked" rather than "what must not be run": a
    /// denylist of destructive spellings is exactly the thing a new `dl`
    /// subcommand walks around. What makes all three safe is that none of them
    /// names a workspace, which is where every destructive `dl` takes its
    /// target.
    ///
    /// `--version` is on the list because reading a listing now depends on
    /// knowing which `dl` wrote it (`reap::answered_where_dl_answers`). It is
    /// asked once per process and answers nothing about any particular
    /// workspace.
    fn asked_a_question(argv: &str) -> bool {
        argv == "dl <--ls> <--json>"
            || argv == "dl <--version>"
            || argv.starts_with("gh <api> <graphql> <-F> <owner=blooop> <-F> <name=wayfinder>")
    }

    /// What the launch probes' `dl` answers `--version` with, either side of
    /// `DEVLAUNCH_FLOOR`.
    ///
    /// Deliberately not adjacent to the floor. `probe::SHIMMED_DL` is 0.0.24
    /// and so is the floor today, so a probe left on the default would assert
    /// isolation from an equality — and the floor's own documentation says it
    /// is raised whenever `wf` starts calling something an older `dl` does not
    /// have. The next bump would then turn `picker_launch_probe` red with a
    /// message about isolation, which is not what went wrong. A version far
    /// above survives every plausible bump; one far below stays below it.
    ///
    /// `0.0.23` is not arbitrary: it is the release the floor exists to
    /// exclude, the one that satisfied every other condition in
    /// `Isolation::detect` and then failed inside the prewarm on an argument it
    /// had never heard of.
    const ABOVE_THE_FLOOR: &str = "9999.0.0";
    const BELOW_THE_FLOOR: &str = "0.0.23";

    /// The same, for the run that ends in a launch.
    fn a_launching_session() -> probe::Recording {
        probe::record_as_dl(
            "picker::tests::picker_launch_probe",
            probe::DL_LISTING,
            probe::GH_FACTS,
            ABOVE_THE_FLOOR,
        )
    }

    #[test]
    fn no_deletion_survives_a_whole_session_of_the_real_assembly() {
        // #137's safety claim as a fact about a run, over the assembly rather
        // than over one expression in it. Between the composition site and the
        // last frame there is exactly one thing this may ask of the machine:
        // what exists, and what the tracker says about it. A `dl <ws> rm` added
        // anywhere in that span — in `run`'s loop, in the drain loop, inside a
        // `tokio::spawn`, behind a helper in another file named nothing anyone
        // thought to forbid — is recorded here, because destroying a workspace
        // means running `dl`. And a destruction that runs no command at all is
        // caught by the scratch home, which must come back untouched.
        //
        // Bounded, and the bound is [`WATCH`]: this is what the run *did*, not
        // what the code cannot do. A cleanup that waits longer than the probe
        // does is not here.
        let run = a_session();
        run.destroyed_nothing();
        // Stated as a rule over every call rather than as a fixed list. "These
        // two questions and no others" is the safety claim, and it has to hold
        // however many times the run asks them — a rule keeps saying that if
        // anything ever asks again, which is the half of this assertion that
        // does not depend on today's keybindings.
        for argv in &run.argv {
            assert!(
                asked_a_question(argv),
                "a session asks for the listing and the tracker, and nothing else: {argv}"
            );
        }
        // And it asks them **once**. This used to be two, because the script
        // pressed the refresh key and it took the reading again; with that key
        // retired there is one reading per session, and a second `dl` here
        // would mean something started asking on its own. Read off the run
        // rather than from the call site: the pair does not repeat.
        assert_eq!(
            run.argv
                .iter()
                .filter(|argv| *argv == "dl <--ls> <--json>")
                .count(),
            1,
            "one reading per session, taken at startup and never again: {:?}",
            run.argv
        );
    }

    #[test]
    fn no_deletion_survives_the_arm_that_launches_either() {
        // The same claim over the fourth arm, which is the one that matters
        // most and the one nothing drove until now. `Outcome::Launch` is where
        // a "free the disk on the way out" cleanup goes: the screen is about to
        // be given back, the picker knows exactly which workspaces a reap would
        // claim, and after the `exec` there is no `wf` left to be blamed. The
        // run this reads ends in that arm, and it asks the machine the same two
        // questions and no others.
        //
        // The arm also shuts the background work down, so a `dl` or `gh` still
        // in flight would land in this log after the reading did.
        let run = a_launching_session();
        run.destroyed_nothing();
        for argv in &run.argv {
            assert!(
                asked_a_question(argv),
                "launching asks nothing extra of the machine: {argv}"
            );
        }
        assert_eq!(run.argv[0], "dl <--ls> <--json>");
        // Bounded as well as filtered. A predicate over each call says nothing
        // about how many there are, and "nothing extra" is a claim about the
        // count — fifty tracker reads on the launch path would satisfy every
        // assertion above. One reading — a listing, the version behind it, and
        // one batched query — and the script does not type until that reading
        // has reached the screen, so all three have landed before the launch
        // arm is anywhere near being entered. It was a loose ceiling while the
        // refresh key was in the plan, because the second reading raced
        // `stop_background`; with one reading the count is exact.
        assert_eq!(
            run.argv.len(),
            3,
            "the one reading this session takes, and nothing the launch added: {:?}",
            run.argv
        );
    }

    #[test]
    fn the_version_floor_is_what_decides_whether_a_launch_is_isolated() {
        // Two runs of the same three keys against the same checkout, differing
        // in one thing: the release their `dl` says it is. One lands in a
        // container, the other on the host. The assertions are the probe
        // bodies' own — `probe::record_as_dl` panics when a child fails — so
        // what this test contributes is the *pair*.
        //
        // The pair is the point, and the first attempt at this guard is why.
        // It asserted that a launching run asked `dl <--version>`, which is
        // true and proves nothing: the startup listing asks it, the answer is
        // memoized in a `OnceLock`, and the record lands in the log whether or
        // not `Isolation::detect` ever consults it. Deleting the floor check
        // outright left that assertion green. A conditional is a claim about
        // two inputs; only two runs can hold it.
        //
        // `DL_LISTING_UNSAVED` for the old one because the fixture has to be a
        // listing that release could have written — it is the one collecting
        // every spelling of `unsaved`, including the bare string 0.0.23 emitted
        // before the field became an object. See `probe::SHIMMED_DL`.
        let _new = probe::record_as_dl(
            "picker::tests::picker_launch_probe",
            probe::DL_LISTING,
            probe::GH_FACTS,
            ABOVE_THE_FLOOR,
        );
        let old = probe::record_as_dl(
            "picker::tests::picker_old_dl_launch_probe",
            probe::DL_LISTING_UNSAVED,
            probe::GH_FACTS,
            BELOW_THE_FLOOR,
        );

        // And the safety sweep over the *host* arm, which is here because this
        // is the only run that reaches it. Before the launch probe grew a
        // devcontainer, `no_deletion_survives_the_arm_that_launches_either`
        // swept a host launch; adding one moved that run to the isolated arm
        // and left the host arm — still the arm most launches take, on a
        // machine with no `dl` or a repo with no devcontainer — with no
        // run-level guard at all. The same two claims as there, over the
        // recording this test already pays for.
        old.destroyed_nothing();
        for argv in &old.argv {
            assert!(
                asked_a_question(argv),
                "a degraded launch asks nothing extra of the machine either: {argv}"
            );
        }
    }

    #[test]
    fn the_first_frame_is_drawn_before_anything_is_asked() {
        // #137's other property, pinned rather than asserted. The reading costs
        // a subprocess and a round trip; the first frame must not be behind
        // either. The script writes its note the first time the loop asks it
        // for a key, which is the instant after the first frame is painted,
        // into the same log the shims append to — so the ordering is a recorded
        // fact: awaiting the reading anywhere in `session` before the loop
        // starts, which *does* compile, puts `dl <--ls> <--json>` above the
        // note and fails here.
        //
        // Read off the timeline rather than the argv, because the note is the
        // one line in it the code under test did not write.
        let run = a_session();
        assert_eq!(
            run.timeline.first().map(String::as_str),
            Some("note| <the first frame>"),
            "the first frame waited for something: {:?}",
            run.timeline
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
        let run = a_session();
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
    fn the_picker_names_neither_a_subprocess_nor_reap() {
        // What is left for source text to say, and it is worth being exact
        // about each token, because a list that claims to be a separation is
        // how three of this PR's four rounds were spent.
        //
        // `reap` is the one that matters. `wf::reap`'s `remove` is private, in
        // a different crate, so no helper, alias or submodule can call it and
        // the edit that tries is a compile error. But `wf::reap::run` is
        // public — `main` has to dispatch `wf reap` — and `reap::run(true,
        // true)` *is* the forced deletion, waiver and all. Nothing about
        // visibility stops this file calling it, so this line does. Bare rather
        // than `reap::`, because the module cannot be reached without its own
        // name being written somewhere: `use wf::reap as tidy;` is caught here
        // too, which `reap::` alone would miss.
        //
        // `Command` and `process::` are for the subprocess the run above
        // cannot see — one started after the probe stops watching. A
        // screen-painting module has no use for either. `tokio::spawn` is on
        // the list because a spawned task is how an `.await` gets into
        // [`Picker::tick`] and [`fold`], neither of which is async, and a task
        // that outlives the loop outlives the probe with it.
        //
        // `fs` is here because the probe drives [`session`], and [`run_picker`]
        // is the function *above* it — composed of a registration, a terminal,
        // the `session` call and the handover, and executed by no test in this
        // repository. A reviewer planted `std::fs::remove_dir_all` in
        // `run_picker` aimed at the real layout
        // (`~/.cache/devlaunch/repos/<owner>/<repo>/<id>` and
        // `~/.devpod/contexts/default/workspaces/<id>`); it named none of the
        // four tokens above, the whole selection was green, and driven under a
        // pty it destroyed a workspace while the picker drew normally.
        //
        // Bare `fs`, and that is the second attempt. The token went on as
        // `fs::`, and the next reviewer reopened the same escape with one line:
        // `use std::fs as sys;` writes no `fs::` anywhere, and
        // `sys::remove_dir_all(…)` in `run_picker` was green again and really
        // destroyed the fixture. It is the same lesson `reap` above is written
        // from — the module cannot be reached without its own name being
        // written — and it was not applied here the first time. Measured:
        // `fs::` in the list is green against the alias and the bare name is
        // bins 16/1; bare `fs` costs this file nothing *today*, because `fs`
        // does not occur in its code at all, in an identifier or anywhere else.
        //
        // That "today" is the cost, and it is worth writing down rather than
        // discovering: this is a substring match, so bare `fs` also forbids
        // `offset`, `refs` and `prefs`. Measured — `let offset = 0usize;` in
        // this file is bins 16/1, reporting *it names "fs"*, which is a
        // misleading message for an edit that reaches nothing. This is a
        // scrolling list, so a scroll offset is a plausible future edit, and
        // the maintainer who hits it will be told the wrong thing. It is the
        // same class of cost `remove` already imposes (see `app.failed`
        // above, which is written `retain` for exactly this reason); the
        // token stays because an argv-less deletion is worse than a confusing
        // test failure, but the next person should not have to rediscover why
        // their `offset` is red.
        // So what this token covers is the part of this file the run does not
        // reach. It is *not* a claim that nothing on the picker's path can
        // delete, and a grep over one file could never be one. The same call
        // spelt in `app.rs` or in a submodule of this module names none of
        // these and compiles; what stands against it there is the recorded argv
        // and the scratch home the run above compares — and outside that home,
        // outside [`session`], or after that run, nothing does, which is true
        // of any code in any file.
        //
        // The list is five tokens, against six in [`wf::reclaim`], seven in
        // [`wf::refresh`] and seven in [`wf::liveness`], and the difference is
        // worth writing down rather than rounding off. `liveness` is the newest
        // and forbids seven of the eight tokens the four lists use between
        // them. The exception is the interesting one: `liveness` cannot forbid
        // `reap`, because it is written on `use crate::reap::{node_of, Node,
        // NodeFact, Workspace}` — it reads that module's *types*. What stands
        // there instead is the same thing that stands in `reclaim`, which calls
        // `reap::plan` by name for the same reason: `reap`'s deletion is
        // private to `reap`, so no import can reach it. Both siblings forbid `remove`, `"rm"` and `--force`;
        // this file forbids none of the three. `remove` it cannot:
        // `app.failed.remove(&id)` is `App`'s own bookkeeping, one occurrence,
        // and renaming a `HashMap` method to satisfy a grep is the distortion a
        // previous round was asked to undo. `"rm"` and `--force` are absent
        // from this file's code and could be added at no cost; they are the
        // argv of the subprocess `Command` and `process::` already forbid
        // spelling, and the recorded run reads argv directly. Going the other
        // way, `tokio::spawn` is on this list and on neither sibling's, because
        // a task spawned here outlives the loop and the probe with it; `reap`
        // is here and in `refresh`'s list but not in `reclaim`'s, which calls
        // `reap::plan` and `reap::doomed` by name and pins that call instead.
        let code = probe::code_only("picker.rs", include_str!("picker.rs"));
        for forbidden in ["reap", "Command", "process::", "tokio::spawn", "fs"] {
            assert!(
                !code.contains(forbidden),
                "the picker draws a screen and drains a channel: it names {forbidden:?}"
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
