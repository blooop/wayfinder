//! The launch: a picked ticket becomes the agent, running right here.
//!
//! `wf` is a selector (#26/#34). There is no multiplexer, no tab, no session
//! and no supervision: the picked node resolves to a checkout, `wf` gives the
//! terminal back, and its own process image is replaced by
//! `claude --dangerously-skip-permissions "<skill> …"` in that checkout
//! ([`Launch::exec`]) — which skill is [`route`]'s answer to what was picked
//! ([`Aim`]) and who decides ([`Mode`]), and any steering text rides the
//! prompt as a suffix (#61/#62/#96). Unattended work is still not supervised
//! here — an `auto` launch is the same exec of `/wf-auto`, watched from
//! another terminal or not at all. One mode invokes no skill at all
//! ([`Mode::Plain`], #112): the same exec, in the same workspace, with the
//! prompt left to the human — so `wf` remains the thing that resolves a node
//! to a place to work even when nothing is going to be run in it.
//!
//! A checkout that declares a devcontainer runs that same agent *inside* a
//! container, by way of `dl` ([`Isolation`], #80): `wf` owns which ticket,
//! which checkout, which skill and which prompt, and `dl` owns the container,
//! its lifecycle and its credentials. The seam is a **per-node workspace**,
//! `owner/repo@wayfinder/<repo>-<n>` ([`Launch::agent_argv`]): every launched
//! node gets its own branch, its own clone under `dl`'s cache and its own
//! container, so any number of tickets run at once without colliding — and
//! the tree the human picked in the checkout picker stays theirs, never
//! mutated by an agent. (#80 originally handed `dl` the checkout path so the
//! agent worked the picked tree; that seam made every launch of a repo share
//! one tree and one container, which is exactly the collision this replaces.)
//! The checkout keeps two jobs: declaring the devcontainer, and hosting
//! host-mode launches.
//!
//! The one thing that can go wrong is ordering: the terminal must be restored
//! *before* the image is replaced, because after that there is no `wf` left to
//! do it. So this module never restores anything and never `exec`s itself off
//! its own initiative — it hands [`Launch`] to the binary, which restores and
//! then calls [`Launch::exec`] as its last act.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{MapId, Stage, Ticket, TicketType};
use crate::projects::Checkout;

/// Which skill the launched agent runs — the (aim, mode) → skill table,
/// hardcoded in `wf` (#61/#96): not per-ticket config, not a Notes-parsed table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `/wf-tdd <n>` — build work: ready, resuming, or acting on red checks and
    /// requested changes (the reviewer's comments live on the PR).
    Tdd,
    /// `/wf-review <n>` — a build whose PR awaits its independent look.
    Review,
    /// `/wf <map> [<n>]` — a decision session with the human in it,
    /// on one ticket or on the map itself.
    Wayfinder,
    /// `/wf-auto <map> [<n>]` — the same map with nobody in the loop:
    /// decisions settled against the skill's guiding principles, and the whole
    /// remaining lifecycle driven unattended (#96).
    WayfinderAuto,
    /// `/wf-one <task>` — one tracked ticket, filed and driven by the skill:
    /// its own single-ticket map, `/wf-tdd` build, `/wf-review` review (#114).
    /// The only route reached by a creation candidate rather than a mode.
    One,
    /// `claude` — no skill at all (#112). The only route that invokes nothing:
    /// the session opens in the node's workspace and the human drives it.
    Plain,
}

impl Route {
    /// How the route reads in the launch picker: the slash command it execs,
    /// or — for the one route that execs none — the agent's own name.
    ///
    /// Every *skill* label is prefixed `wf`, because those names are claimed in
    /// a namespace `wf` does not own: `~/.claude/skills` is flat and shared with
    /// every other source of skills the user has. Unprefixed `tdd` and `review`
    /// are names someone else will plausibly want — and while `wf` holds them,
    /// nobody else can have them (#104). [`Route::Plain`] is outside that
    /// argument entirely: it claims no name, because it invokes nothing.
    pub fn label(self) -> &'static str {
        match self {
            Route::Tdd => "/wf-tdd",
            Route::Review => "/wf-review",
            Route::Wayfinder => "/wf",
            Route::WayfinderAuto => "/wf-auto",
            Route::One => "/wf-one",
            Route::Plain => "claude",
        }
    }

    /// The bundled skill this route invokes, named as
    /// [`crate::skills::BUNDLED`] spells it — `None` for the one route that
    /// invokes no skill.
    ///
    /// The typed form of "adding a `Route` means adding a skill": a new route
    /// has to say which bundled prompt it names, or that it names none, and
    /// cannot quietly point at one the package does not ship. Exhaustive, so
    /// the answer is given at the point the route is added rather than
    /// discovered at an agent launch.
    pub fn bundled_skill(self) -> Option<&'static str> {
        match self {
            Route::Tdd | Route::Review | Route::Wayfinder | Route::WayfinderAuto | Route::One => {
                Some(self.label().trim_start_matches('/'))
            }
            Route::Plain => None,
        }
    }

    /// The next route, wrapping — private, and existing only so [`Route::all`]
    /// can be derived rather than written out. Same device as [`Mode::after`],
    /// for the same reason: a list written beside the enum is a second place to
    /// remember, and the bundle invariant is exactly what forgetting it breaks.
    fn after(self) -> Route {
        match self {
            Route::Tdd => Route::Review,
            Route::Review => Route::Wayfinder,
            Route::Wayfinder => Route::WayfinderAuto,
            Route::WayfinderAuto => Route::One,
            Route::One => Route::Plain,
            Route::Plain => Route::Tdd,
        }
    }

    /// Every route there is. Walks the `after` cycle until it comes back round
    /// — or, if a future cycle is malformed and never does, until it repeats
    /// itself, so this cannot spin.
    pub fn all() -> Vec<Route> {
        let mut routes = vec![Route::Tdd];
        while let Some(&last) = routes.last() {
            let next = last.after();
            if routes.contains(&next) {
                break;
            }
            routes.push(next);
        }
        routes
    }
}

/// Who resolves the launched node — the axis #96 added to routing, orthogonal
/// to what the cursor was standing on.
///
/// Not a flag on the skill: the modes are *different skills* (`/wf` and
/// `/wf-auto`) — or, in [`Mode::Plain`]'s case, no skill at all — so the mode
/// is an input to [`route`] rather than something the prompt carries. That is
/// why nothing about "auto" survives into the exec'd prompt's steering suffix:
/// by then it has already been spent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// The default: the human is in the loop, and the session grills them.
    #[default]
    Interactive,
    /// The agent decides alone, under `/wf-auto`'s declared principles, and
    /// drives the node's remaining lifecycle unattended. Replaced `defer`,
    /// which routed to `/wf`'s own deferred mode before that skill existed
    /// (#63 → #96).
    Auto,
    /// Nobody: no skill is invoked and no lifecycle is driven. The session
    /// opens on the node's workspace and the human types the first thing it
    /// hears (#112) — the answer to wanting `wf`'s branch, clone and container
    /// without wanting a skill's opinion about what to do in them.
    Plain,
}

impl Mode {
    /// How the mode reads in the launch picker.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Interactive => "interactive",
            Mode::Auto => "auto",
            Mode::Plain => "plain",
        }
    }

    /// What picking it means, in the words the choice actually turns on: who
    /// decides. The picker shows this because the skill name alone (`/wf` vs
    /// `/wf-auto`) does not say that one of them will not stop to ask you.
    pub fn blurb(self) -> &'static str {
        match self {
            Mode::Interactive => "you are in the loop; it grills you",
            Mode::Auto => "the agent decides alone and drives it to done",
            Mode::Plain => "no skill; a bare session on the node's branch",
        }
    }

    /// The next mode in the picker's order, wrapping.
    ///
    /// Exhaustive, and the reason [`Mode::all`] is derived from it rather than
    /// written out beside it: a new mode has to say where it sits in the cycle,
    /// and the picker then lists it without anyone having to remember a second
    /// place to add it.
    fn after(self) -> Mode {
        match self {
            Mode::Interactive => Mode::Auto,
            Mode::Auto => Mode::Plain,
            Mode::Plain => Mode::Interactive,
        }
    }

    /// Every mode, in picker order, starting at the default. Walks the private
    /// `after` cycle until it comes back round — or, if a future cycle is
    /// malformed and never does, until it repeats itself, so this cannot spin.
    pub fn all() -> Vec<Mode> {
        let mut modes = vec![Mode::default()];
        while let Some(&last) = modes.last() {
            let next = last.after();
            if modes.contains(&next) {
                break;
            }
            modes.push(next);
        }
        modes
    }
}

/// Something new to start in a repo — the creation half of the picker (#114).
/// Which kind, not yet what: the typed text that completes it (the task, or a
/// map seed) arrives at the second enter, as [`Creation`].
///
/// Three hand-listed kinds rather than a creation × [`Mode`] product, because
/// the product is dishonest: charting a map with no skill (`plain`) is
/// meaningless, and a task's lifecycle is `/wf-one`'s own. These are exactly
/// the combinations that mean something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationKind {
    /// One tracked ticket via `/wf-one`: filed, built, reviewed.
    Task,
    /// Chart a new map via `/wf`, with the human in the loop.
    Map,
    /// Chart a new map via `/wf-auto`, alone.
    MapAuto,
}

impl CreationKind {
    /// Every kind, in picker order. Written out rather than derived from an
    /// `after` cycle: three variants with one call site is below the size
    /// where the cycle device earns its ceremony.
    pub fn all() -> Vec<CreationKind> {
        vec![CreationKind::Task, CreationKind::Map, CreationKind::MapAuto]
    }

    /// How the row reads in the picker.
    pub fn label(self) -> &'static str {
        match self {
            CreationKind::Task => "new task",
            CreationKind::Map => "new map",
            CreationKind::MapAuto => "new map, auto",
        }
    }

    /// What picking it means — same register as [`Mode::blurb`].
    pub fn blurb(self) -> &'static str {
        match self {
            CreationKind::Task => "one tracked ticket, built and reviewed",
            CreationKind::Map => "chart a new map in this repo, with you",
            CreationKind::MapAuto => "chart a new map in this repo, alone",
        }
    }

    /// The skill this creation execs. Its own answer rather than [`route`]'s:
    /// creation has no aim and no stage, so the (aim, mode) table has nothing
    /// to say about it.
    pub fn route(self) -> Route {
        match self {
            CreationKind::Task => Route::One,
            CreationKind::Map => Route::Wayfinder,
            CreationKind::MapAuto => Route::WayfinderAuto,
        }
    }

    /// What the text field means on this row — the name drawn beside it.
    pub fn field(self) -> &'static str {
        match self {
            CreationKind::Task => "task",
            CreationKind::Map | CreationKind::MapAuto => "seed",
        }
    }

    /// Complete this kind with the typed text — parse, don't validate. `None`
    /// is the one refusal: a task with nothing typed, because `/wf-one` with
    /// no task is meaningless and the picker refuses it on the count line the
    /// way a done or blocked node already refuses. A map's text is only a
    /// seed, so absence is fine and is spelt `None` rather than `Some("")`.
    pub fn with_text(self, text: &str) -> Option<Creation> {
        let text = text.trim();
        let seed = || (!text.is_empty()).then(|| text.to_string());
        match self {
            CreationKind::Task => (!text.is_empty()).then(|| Creation::Task {
                task: text.to_string(),
            }),
            CreationKind::Map => Some(Creation::Map { seed: seed() }),
            CreationKind::MapAuto => Some(Creation::MapAuto { seed: seed() }),
        }
    }
}

/// A creation, completed: the kind plus the text that makes it launchable
/// (#114). Built only by [`CreationKind::with_text`], so a task without its
/// text is unrepresentable — the refusal happened at the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Creation {
    /// `/wf-one <task>` — the task text, never empty.
    Task { task: String },
    /// `/wf [<seed>]` — charting with the human; the seed is the loose idea.
    Map { seed: Option<String> },
    /// `/wf-auto [<seed>]` — charting alone.
    MapAuto { seed: Option<String> },
}

impl Creation {
    /// The kind this creation completes — for the labels the notice reuses.
    fn kind(&self) -> CreationKind {
        match self {
            Creation::Task { .. } => CreationKind::Task,
            Creation::Map { .. } => CreationKind::Map,
            Creation::MapAuto { .. } => CreationKind::MapAuto,
        }
    }

    /// The whole prompt this creation execs. Built here rather than through
    /// [`LaunchMode::opening_prompt`] because nothing is being steered: the
    /// text is the skill's *argument* — the task, or the loose idea — not a
    /// ` steer: …` suffix on a session that already has a subject.
    fn invocation(&self) -> String {
        let seeded = |skill: &str, seed: &Option<String>| match seed {
            None => skill.to_string(),
            Some(seed) => format!("{skill} {seed}"),
        };
        match self {
            Creation::Task { task } => format!("{} {task}", Route::One.label()),
            Creation::Map { seed } => seeded(Route::Wayfinder.label(), seed),
            Creation::MapAuto { seed } => seeded(Route::WayfinderAuto.label(), seed),
        }
    }
}

/// One row of the launch picker: a **complete candidate**, carrying its own
/// aim and mode and therefore its own resolved route (#114). The picker's
/// list stopped being a bare [`Mode`] walk when it started answering two
/// questions — what am I aiming at, and who resolves it — and complete rows
/// are what keep every one of them naming the skill it execs.
///
/// The launch arm carries its route *resolved* rather than re-deriving it at
/// each use — the [`Targets::Many`] move: the pick cannot produce a launch
/// inconsistent with the row that was drawn. [`Staged::candidates`] is the
/// one constructor, so a launch row whose route disagrees with its mode, or a
/// creation row on a stop that is not repo-level, is never built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    /// Launch the staged node, `mode` deciding who resolves it.
    Launch { mode: Mode, route: Route },
    /// Start something new in the staged node's repo.
    Create(CreationKind),
}

impl Candidate {
    /// How the row reads in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Candidate::Launch { mode, .. } => mode.label(),
            Candidate::Create(kind) => kind.label(),
        }
    }

    /// What picking it means.
    pub fn blurb(self) -> &'static str {
        match self {
            Candidate::Launch { mode, .. } => mode.blurb(),
            Candidate::Create(kind) => kind.blurb(),
        }
    }

    /// The skill this row execs — already resolved, whichever arm.
    pub fn route(self) -> Route {
        match self {
            Candidate::Launch { route, .. } => route,
            Candidate::Create(kind) => kind.route(),
        }
    }

    /// What the text field means while this row is picked: launch rows steer
    /// the agent; creation rows take the task or the seed.
    pub fn field(self) -> &'static str {
        match self {
            Candidate::Launch { .. } => "steer",
            Candidate::Create(kind) => kind.field(),
        }
    }
}

/// What the launch picker settled on: who decides, and what steers them.
///
/// Two independent axes, so genuinely a product and not a four-armed sum: the
/// picked [`Mode`] chooses *who decides*, and the steering text — typed in the
/// same overlay — steers them. Every combination is meaningful, which is the
/// test for a product being honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchMode {
    mode: Mode,
    /// The steering prompt, never empty — [`LaunchMode::picked`] trims, and an
    /// empty steer is spelt `None` rather than `Some("")`.
    steer: Option<String>,
}

impl LaunchMode {
    /// The launch the picker composed: a mode selected from a list, and
    /// whatever steering text was typed alongside it.
    ///
    /// The mode is *picked*, not parsed out of the text (#62's `defer`, then
    /// #96's `auto`, were both words at the front of one line). Nothing you can
    /// type changes who decides, so steering text starting `auto` is steering
    /// text — and, the other way round, an unattended launch is a thing you
    /// selected and saw selected rather than a word you had to know.
    pub fn picked(mode: Mode, steer: &str) -> LaunchMode {
        let steer = steer.trim();
        LaunchMode {
            mode,
            steer: (!steer.is_empty()).then(|| steer.to_string()),
        }
    }

    /// Which skill this launch resolves to, given what the cursor was on.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// What the agent is opened on, given the skill invocation the route
    /// resolved to — `None` when there is nothing to say to it at all.
    ///
    /// Both halves of the steering axis are answered here rather than half of
    /// them at the call site, because the typed text means something different
    /// depending on whether anything is in front of it: with a skill it is a
    /// ` steer: <text>` suffix *on* that skill, and with none there is nobody
    /// for a suffix to be addressed to, so it is simply the whole prompt. The
    /// mode itself is in neither — it has already been spent choosing the
    /// route.
    fn opening_prompt(&self, skill: Option<String>) -> Option<String> {
        match (skill, &self.steer) {
            (Some(skill), None) => Some(skill),
            (Some(skill), Some(text)) => Some(format!("{skill} steer: {text}")),
            (None, steer) => steer.clone(),
        }
    }
}

/// A stage a launch can act on: [`Stage`] minus [`Stage::Done`], which has no
/// work left to hand an agent.
///
/// Parsed once, at the first enter ([`Launchable::parse`]), so that a staged
/// launch of a finished node is unrepresentable and [`route`] is **total** —
/// no `Option` for the three later steps to carry, wonder about and re-refuse.
/// Blocked never reaches here at all: blocked is [`crate::model::Status`], not
/// a stage, and the picker refuses it before anything is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launchable {
    Ready,
    Building,
    InReview,
    NeedsAttention,
}

impl Launchable {
    /// Narrow a derived stage to one an agent can be launched on. Exhaustive
    /// on [`Stage`]: a new stage must decide whether it is launchable, or this
    /// stops compiling.
    pub fn parse(stage: Stage) -> Option<Launchable> {
        match stage {
            Stage::Ready => Some(Launchable::Ready),
            Stage::Building => Some(Launchable::Building),
            Stage::InReview => Some(Launchable::InReview),
            Stage::NeedsAttention => Some(Launchable::NeedsAttention),
            Stage::Done => None,
        }
    }
}

/// What a launch is aimed at: a whole map, or one ticket in it (#96).
///
/// The cursor can land on a cluster header now, and a map is not a ticket with
/// a missing number — it has no type and no stage, because stage is derived
/// from a node's PRs and a map has none. So the two are arms of one sum rather
/// than a ticket struct with optional fields, and every consumer that needs a
/// ticket number has to say which case it is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// The cluster header: the map itself, charted or driven as a whole.
    Map,
    /// One ticket in the map, carrying the pair its route turns on.
    Ticket {
        number: u64,
        ticket_type: TicketType,
        stage: Launchable,
    },
}

/// Resolve which skill a launch runs, from what it is aimed at and who is
/// deciding. Total on every axis (#96): adding a stage, a type or a mode
/// without saying where it routes is a compile error, not a fall-through.
///
/// The mode axis collapses the ticket table wherever it is `Auto`, and that is
/// the point rather than a shortcut: under `Auto` the launched session is a
/// **manager**, and what it manages is the node's whole remaining lifecycle —
/// `/wf-tdd`, the gate, then fresh-context `/wf-review` — so it is `/wf-auto`
/// that gets launched at every stage, not the stage's own skill.
pub fn route(aim: &Aim, mode: Mode) -> Route {
    match (aim, mode) {
        (Aim::Map, Mode::Interactive) => Route::Wayfinder,
        (Aim::Map | Aim::Ticket { .. }, Mode::Auto) => Route::WayfinderAuto,
        // `Plain` collapses the table for the opposite reason `Auto` does:
        // `Auto` picks one skill for every node, `Plain` picks none, and
        // neither the aim nor the stage can change that.
        (Aim::Map | Aim::Ticket { .. }, Mode::Plain) => Route::Plain,
        (
            Aim::Ticket {
                ticket_type, stage, ..
            },
            Mode::Interactive,
        ) => match stage {
            // Build rows of the #61 table: in-review hands off to the fresh-eyes
            // reviewer; everything else on a build node is code work.
            Launchable::InReview => match ticket_type {
                TicketType::Build => Route::Review,
                TicketType::Research
                | TicketType::Task
                | TicketType::Grilling
                | TicketType::Prototype
                | TicketType::Untyped => Route::Wayfinder,
            },
            Launchable::Ready | Launchable::Building | Launchable::NeedsAttention => {
                match ticket_type {
                    TicketType::Build => Route::Tdd,
                    // Decision types (untyped riding along, as it always
                    // launched): /wf at every unfinished stage —
                    // PR-dominant derivation can put a decision node past "in
                    // progress" (a prototype's PR counts), and the skill owns
                    // its node's PR state.
                    TicketType::Research
                    | TicketType::Task
                    | TicketType::Grilling
                    | TicketType::Prototype
                    | TicketType::Untyped => Route::Wayfinder,
                }
            }
        },
    }
}

/// A launch the first enter staged but the machine has not answered yet (#62):
/// everything the launch picker draws and the second enter needs, snapshotted
/// **index-free**.
///
/// Index-free is the whole point. `crate::app::Row` is positional — an index
/// into a `Vec` that the next fetch replaces — and the picker stays up while
/// background map arrivals swap the clusters underneath it (#27). A `Row` held
/// here would draw, and then launch, whichever ticket had landed at that
/// index; a shorter map would panic on the next frame. So the staged launch
/// carries the ticket's own facts, the way [`Targets::Many`] carries complete
/// [`Launch`]es rather than a choice to re-resolve later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The repo, full slug (`owner/name`) — what the checkout cache is
    /// matched on (#15), and the repo any creation lands in.
    pub repo: String,
    /// What was stood on: a node fetched from the tracker, or a registered
    /// repo with no map at all.
    pub at: StagedAt,
    /// Its title as it read when the picker opened — the picker is showing the
    /// human what they picked, not re-reporting a row that may have moved.
    pub title: String,
}

/// What the picker was opened on (#114). A map-less repo is not a node with a
/// missing issue number — it has no aim, no map and nothing to launch, because
/// nothing has been filed in it yet — so the two are arms of one sum rather
/// than a node struct with optional fields. Which rows the picker offers falls
/// out of this: a node launches (and a map also creates), a bare repo only
/// creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedAt {
    /// A node the tracker knows about.
    Node {
        /// What the launch picker names: the map, or one ticket in it.
        aim: Aim,
        /// The map issue of the cluster the row was picked in (#50) — which
        /// map a ticket listed twice was launched from, and the launch target
        /// itself when the cursor was on the cluster header.
        map_issue: u64,
    },
    /// A registered checkout whose repo has no open map: the empty-state door
    /// this repo's *first* map is charted from.
    Project,
}

impl Staged {
    /// Stage a launch of `ticket`, picked in the cluster of `map_issue`.
    /// `None` for a finished ticket, which has no launchable stage — the one
    /// refusal, made here so that everything downstream is total.
    pub fn ticket(ticket: &Ticket, map_issue: u64, stage: Stage) -> Option<Staged> {
        Some(Staged {
            repo: ticket.repo.clone(),
            at: StagedAt::Node {
                aim: Aim::Ticket {
                    number: ticket.number,
                    ticket_type: ticket.ticket_type,
                    stage: Launchable::parse(stage)?,
                },
                map_issue,
            },
            title: ticket.title.clone(),
        })
    }

    /// Stage a launch of a whole map — the cursor was on its cluster header.
    /// Total, unlike the ticket case: a map has no stage to be finished at,
    /// and a finished map is not on screen to put the cursor on.
    pub fn map(id: &MapId, title: &str) -> Staged {
        Staged {
            repo: id.repo.clone(),
            at: StagedAt::Node {
                aim: Aim::Map,
                map_issue: id.number,
            },
            title: title.to_string(),
        }
    }

    /// Stage the empty-state door: a registered repo with no open map (#114).
    /// There is no node, so the picker offers creation alone.
    pub fn project(repo: &str) -> Staged {
        Staged {
            repo: repo.to_string(),
            at: StagedAt::Project,
            title: "no map".to_string(),
        }
    }

    /// Which skill launching this node in `mode` would run — `None` for the
    /// map-less door, which has no node to launch and therefore no route.
    pub fn route(&self, mode: Mode) -> Option<Route> {
        match &self.at {
            StagedAt::Node { aim, .. } => Some(route(aim, mode)),
            StagedAt::Project => None,
        }
    }

    /// The picker's rows for this stop, in on-screen order — the one
    /// constructor of [`Candidate`], which is what makes an inconsistent row
    /// unbuildable (#114). Every stop launches; only the repo-level stop — the
    /// cluster header, [`Aim::Map`] — adds the creation rows, because creation
    /// is a repo-level act and a ticket picker carrying it would merge
    /// concerns the stop grammar keeps apart.
    /// The row the picker opens on: the default mode's launch row, whatever
    /// else this stop offers. Creation is never the default — `enter` on a
    /// node still means "launch this node" first.
    ///
    /// # Panics
    ///
    /// Never: [`Staged::candidates`] always leads with the launch rows.
    pub fn default_candidate(&self) -> Candidate {
        *self
            .candidates()
            .first()
            .expect("every stop offers its launch rows")
    }

    pub fn candidates(&self) -> Vec<Candidate> {
        let launches = |aim: Aim| {
            Mode::all()
                .into_iter()
                .map(move |mode| Candidate::Launch {
                    mode,
                    route: route(&aim, mode),
                })
                .collect::<Vec<_>>()
        };
        let creations = || CreationKind::all().into_iter().map(Candidate::Create);
        match self.at {
            StagedAt::Node {
                aim: aim @ Aim::Ticket { .. },
                ..
            } => launches(aim),
            StagedAt::Node {
                aim: aim @ Aim::Map,
                ..
            } => launches(aim).into_iter().chain(creations()).collect(),
            // Nothing has been filed in this repo yet, so there is nothing to
            // launch — only the three ways to start something.
            StagedAt::Project => creations().collect(),
        }
    }

    /// How the staged stop reads: `#<n>` for a node — the ticket's number or
    /// the map's — and `+new` for the map-less door, which has no number to
    /// name until a skill files one.
    pub fn key(&self) -> String {
        match &self.at {
            StagedAt::Node {
                aim: Aim::Map,
                map_issue,
            } => format!("#{map_issue}"),
            StagedAt::Node {
                aim: Aim::Ticket { number, .. },
                ..
            } => format!("#{number}"),
            StagedAt::Project => "+new".to_string(),
        }
    }
}

/// Where the agent runs: on the host, as `wf` always has, or inside the
/// checkout's own devcontainer by way of `dl` (#80).
///
/// Two states, not three: there is no "wanted isolation but could not get it".
/// [`Isolation::detect`] is total — it answers with what will actually happen,
/// so a launch cannot carry an intention the exec then fails to honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// No devcontainer in the checkout, or no `dl` on PATH: `claude` runs in
    /// the checkout directly.
    Host,
    /// `dl owner/repo@wayfinder/<repo>-<n> -- claude …`: the agent runs in the
    /// container the repo's own `devcontainer.json` describes, in a workspace
    /// of its own — one branch, one clone, one container per node.
    Devlaunch,
}

/// The devcontainer configs `wf` looks for — the two locations the
/// devcontainer spec puts a *default* config in.
///
/// A variant-only layout (`.devcontainer/<name>/devcontainer.json` with no
/// default) is deliberately absent: picking among variants would be `wf`
/// choosing a container shape, and `wf` has no basis to choose. Those repos
/// run on the host until someone decides how the variant is named.
const DEVCONTAINER_CONFIGS: [&str; 2] = [".devcontainer/devcontainer.json", ".devcontainer.json"];

/// The container front door, found on PATH like every other tool `wf` uses.
const DEVLAUNCH: &str = "dl";

impl Isolation {
    /// Which environment `checkout` will actually get.
    ///
    /// Both halves are required, and a missing `dl` **degrades to the host**
    /// rather than refusing the launch: a repo may carry a `devcontainer.json`
    /// for its editor users on a machine that has never heard of `dl`, and
    /// isolation here is for dependencies, not security (#73), so the host is
    /// a worse environment rather than an unsafe one. The launch notice names
    /// the mode ([`Launch::describe`]), so the degradation is visible.
    pub fn detect(checkout: &Path) -> Isolation {
        if has_devcontainer(checkout) && resolve_on_path(DEVLAUNCH).is_ok() {
            Isolation::Devlaunch
        } else {
            Isolation::Host
        }
    }

    /// How the mode reads on screen — the launch notice and the checkout
    /// picker, which is where two trees of one repo can differ. The host is
    /// the default and says nothing; anything else has to announce itself.
    pub fn suffix(self) -> &'static str {
        match self {
            Isolation::Host => "",
            Isolation::Devlaunch => " (devlaunch)",
        }
    }
}

/// Whether `checkout` declares a devcontainer `wf` can hand to `dl` as-is.
///
/// Existence only — `wf` never reads a `devcontainer.json`, never parses its
/// JSONC and never rewrites it (#73). What is inside is the repo's business.
fn has_devcontainer(checkout: &Path) -> bool {
    DEVCONTAINER_CONFIGS
        .iter()
        .any(|rel| checkout.join(rel).is_file())
}

/// Wrap one argument so a POSIX shell hands it back unchanged.
///
/// Needed because `dl <ws> -- <cmd>` is a **shell command, not an argv**: `dl`
/// joins everything after `--` with spaces and gives the single string to
/// `devpod ssh --command`, which runs it through a shell inside the container.
/// So [`Launch::agent_argv`]'s one-argv-entry-per-prompt invariant does not
/// survive the trip on its own — unquoted, `/wf 67 80` would arrive as
/// three arguments.
///
/// Single quotes, uniformly, because inside them a POSIX shell interprets
/// nothing at all; the one thing they cannot hold is a single quote, which
/// closes, escapes and reopens. Quoting every argument rather than only the
/// ones that "need" it keeps this total — there is no predicate to get wrong.
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// A fully-resolved launch: which checkout the agent runs in, which ticket of
/// which map it is handed, and — since the two-step (#62) — which skill it
/// runs ([`Route`]) and in what mode ([`LaunchMode`]).
///
/// The fields are private and [`plan`] is the only constructor, so a launch
/// whose checkout belongs to a different repo than its ticket is
/// unrepresentable rather than merely undocumented. The route arrives already
/// resolved from (type, stage) by [`route`], which is where an unlaunchable
/// node was refused — a `Launch` for a done node cannot be built, because no
/// `Route` for it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The repo, full slug (`owner/name`) — the identity half, kept
    /// whole because a fork and its upstream share a short name (#15).
    repo: String,
    /// The picked checkout: the process's working directory, and the agent's
    /// working tree on the host. An isolated launch works in its own `dl`
    /// workspace instead — the checkout's remaining job there is having
    /// declared the devcontainer.
    cwd: PathBuf,
    /// What the agent is asked to do there: work a node, or start something
    /// new (#114).
    job: Job,
    /// Host or container, decided from the checkout at plan time (#80).
    isolation: Isolation,
}

/// What a launch asks of its agent — the sum that keeps a creation from being
/// a node with missing fields (#114). A creation has no aim, no map issue and
/// no stage, because the things it would name do not exist until the launched
/// skill files them; giving it sentinel zeroes would put a lie in every
/// consumer. The workspace rule falls out per arm: a node gets its per-ticket
/// branch, a creation gets the repo's default workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    /// Work a node fetched from the tracker.
    Node {
        /// The map, or one ticket in it.
        aim: Aim,
        /// The map issue — `/wf`'s first argument, and its only one when the
        /// aim is the map itself.
        map_issue: u64,
        /// The skill this launch execs, resolved from (type, stage).
        route: Route,
        /// What the launch picker settled on. The mode half already picked
        /// `route`; what is left to spend here is the steering text.
        mode: LaunchMode,
    },
    /// Start something new in the repo — the skill files the issues.
    Create(Creation),
}

/// Agent sessions are started from a picker rather than from a shell someone is
/// watching, so they do not stop for permission prompts.
const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";

impl Launch {
    /// The checkout the agent runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// How this launch reads on screen: `<short_repo>#<number>` for a node —
    /// the ticket's number, or the map's when the whole map is what was
    /// picked — and `<short_repo>+new` for a creation, which has no number
    /// until the skill files one.
    pub fn key(&self) -> String {
        match &self.job {
            Job::Node { aim, map_issue, .. } => {
                let number = match aim {
                    Aim::Map => map_issue,
                    Aim::Ticket { number, .. } => number,
                };
                format!("{}#{}", short_repo(&self.repo), number)
            }
            Job::Create(_) => format!("{}+new", short_repo(&self.repo)),
        }
    }

    /// The `dl` workspace an isolated launch runs in.
    ///
    /// A node gets `owner/repo@wayfinder/<repo>-<n>` — one branch per node,
    /// so parallel launches never share a tree or a container, and
    /// relaunching a node reattaches to the workspace it already has. The
    /// branch name is not `wf`'s invention: `wayfinder/<repo>-<n>` is the
    /// branch `/wf-tdd` is instructed to do ticket `n`'s work on. `dl` creates
    /// it (locally, off the default branch) when it does not exist yet, so a
    /// build agent wakes up already on its work branch, and a reviewer's
    /// workspace opens on the branch the PR was pushed from.
    ///
    /// A creation has no number, so no per-ticket branch exists to name: it
    /// gets the bare `owner/repo` — the default workspace — and the launched
    /// skill files its own issues and makes its own branches (#114).
    fn workspace(&self) -> String {
        match &self.job {
            Job::Node { aim, map_issue, .. } => {
                let number = match aim {
                    Aim::Map => map_issue,
                    Aim::Ticket { number, .. } => number,
                };
                format!(
                    "{}@wayfinder/{}-{}",
                    self.repo,
                    short_repo(&self.repo),
                    number
                )
            }
            Job::Create(_) => self.repo.clone(),
        }
    }

    /// Where this launch's agent runs.
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }

    /// One-line description for the notice: what is being launched, where, and
    /// — when it is not the host default — in what. "Where" is where the agent
    /// actually works: the checkout on the host, the per-node workspace in a
    /// container.
    pub fn describe(&self) -> String {
        let place = match self.isolation {
            Isolation::Host => self.cwd.display().to_string(),
            Isolation::Devlaunch => self.workspace(),
        };
        let what = match &self.job {
            Job::Node { .. } => self.key(),
            // A creation has no `#n` to name yet, so the notice names the act.
            Job::Create(creation) => creation.kind().label().to_string(),
        };
        format!("{what} in {place}{}", self.isolation.suffix())
    }

    /// The agent itself. `claude` takes a single positional prompt, so the
    /// slash command, its arguments and the steering suffix are one argv
    /// entry, not several — and, when there is nothing to say to it at all, no
    /// entry rather than an empty one (`claude ""` is a prompt, and an empty
    /// one).
    fn claude_argv(&self) -> Vec<String> {
        let mut argv = vec!["claude".to_string(), SKIP_PERMISSIONS.to_string()];
        let prompt = match &self.job {
            Job::Node { mode, .. } => mode.opening_prompt(self.skill_invocation()),
            Job::Create(creation) => Some(creation.invocation()),
        };
        argv.extend(prompt);
        argv
    }

    /// The slash command and its arguments, for the node routes that invoke a
    /// skill. `None` is [`Route::Plain`]: no skill, so nothing to invoke.
    /// A creation never reaches here — its prompt is [`Creation::invocation`].
    ///
    /// [`Route::Plain`] is absorbed first because it is the arm that needs no
    /// second thought — it has no arguments to shape, whatever it was aimed at.
    /// The map aim comes next for the same reason: a skill on a map is the
    /// map's number alone. The ticket arm is where the two argument shapes
    /// live — only the wayfinder skills take the map argument, since `/wf-tdd`
    /// and `/wf-review` resolve the repo from the checkout they run in.
    fn skill_invocation(&self) -> Option<String> {
        let Job::Node {
            aim,
            map_issue,
            route,
            ..
        } = &self.job
        else {
            return None;
        };
        let skill = route.label();
        match (route, aim) {
            (Route::Plain, _) => None,
            // Unreachable from a node: [`route`] never answers `One` — it is
            // the creation candidates' route (#114), and a creation launch
            // builds its prompt from the typed task, not from an aim. If a
            // node ever did carry it, the bare skill grills for its task.
            (Route::One, _) => Some(skill.to_string()),
            (_, Aim::Map) => Some(format!("{skill} {map_issue}")),
            (Route::Tdd | Route::Review, Aim::Ticket { number, .. }) => {
                Some(format!("{skill} {number}"))
            }
            (Route::Wayfinder | Route::WayfinderAuto, Aim::Ticket { number, .. }) => {
                Some(format!("{skill} {map_issue} {number}"))
            }
        }
    }

    /// What `wf` becomes: the agent, or `dl` carrying the agent into the
    /// container.
    ///
    /// The isolated form hands `dl` the per-node workspace
    /// — `owner/repo@branch` — which `dl` answers by cloning that branch under
    /// its own cache (creating it off the default branch if it is new) and
    /// running the agent in a container of its own. That second tree is the
    /// point: N launched nodes are N branches in N containers, colliding
    /// nowhere, and the human's checkout is not one of them. The workspace
    /// spec is a plain argv entry to `dl` and needs no quoting; the agent
    /// command that follows `--` does, because `dl` runs it through a shell
    /// (every argument is single-quoted), and it is one entry rather than
    /// several because "a shell command" is exactly what `dl` documents it
    /// to be.
    pub fn agent_argv(&self) -> Vec<String> {
        let agent = self.claude_argv();
        match self.isolation {
            Isolation::Host => agent,
            Isolation::Devlaunch => vec![
                DEVLAUNCH.to_string(),
                self.workspace(),
                "--".to_string(),
                agent
                    .iter()
                    .map(|arg| shell_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" "),
            ],
        }
    }

    /// Become the agent: replace `wf`'s process image with `claude` — or with
    /// the `dl` that carries it into the container — in the checkout.
    ///
    /// Returns **only** on failure — on success there is no `wf` left to return
    /// to, which is why the return type is a bare error rather than a `Result`
    /// whose `Ok` nobody could ever observe. `exec` rather than spawn-and-wait
    /// is the shape #26 chose: `#5`'s "never `exec`" existed so `wf` could
    /// survive a detach, and with nothing left to survive for a lingering
    /// parent buys nothing while costing the agent its direct hold on the
    /// terminal, the exit code and the signals.
    ///
    /// **The caller must have restored the terminal first.** There is no second
    /// chance after the image is replaced, so that ordering lives in `main`,
    /// where it is one statement above the call, rather than in here.
    ///
    /// # Panics
    ///
    /// Never in practice: [`agent_argv`](Self::agent_argv) builds the vector
    /// literally and always starts it with the program name, so the split below
    /// cannot come up empty. The `expect` is there to say so.
    pub fn exec(&self) -> anyhow::Error {
        let argv = self.agent_argv();
        let (program, rest) = argv.split_first().expect("agent argv is never empty");

        // Resolved against `$PATH` *before* the chdir, deliberately. `exec`
        // chdirs into `cwd` and only then runs `execvp`, so a `$PATH` holding
        // an empty entry — a leading, trailing or doubled `:`, which is an
        // everyday `.bashrc` accident — resolves the agent out of **the
        // checkout**. Cloning a repo and running `wf` in it would be enough to
        // run its `./claude` with `--dangerously-skip-permissions`. Empty
        // entries are dropped rather than read as `.`, which is the one place
        // this deliberately differs from `execvp`.
        let program = match resolve_on_path(program) {
            Ok(program) => program,
            Err(err) => return err,
        };
        // Checked because the two failures are both `ENOENT` and the fix for
        // each is completely different. The cache is pruned once at startup and
        // the picker holds a snapshot, so a `git worktree remove` in another
        // terminal during the session lands here.
        if !self.cwd.is_dir() {
            return anyhow::anyhow!(
                "the checkout {} is gone — nothing to run the agent in",
                self.cwd.display()
            );
        }

        // `CommandExt::exec` only ever returns on failure.
        let err = Command::new(&program)
            .args(rest)
            .current_dir(&self.cwd)
            .exec();
        // Quoted, so the prompt reads as the single argument it is — the whole
        // invariant `agent_argv` exists to hold.
        let quoted: Vec<String> = std::iter::once(program.display().to_string())
            .chain(rest.iter().cloned())
            .map(|a| format!("{a:?}"))
            .collect();
        anyhow::Error::new(err).context(format!(
            "running {} in {}",
            quoted.join(" "),
            self.cwd.display()
        ))
    }
}

/// Find `program` on `$PATH`, skipping empty entries.
///
/// A name containing a separator is a path already and is taken as given —
/// that is the caller naming a file, not `$PATH` resolution.
///
/// Two callers, and the difference matters: [`Launch::exec`] resolves the
/// program it is about to become and reports the miss, while
/// [`Isolation::detect`] only asks whether `dl` is there and quietly answers
/// [`Isolation::Host`] when it is not.
fn resolve_on_path(program: &str) -> Result<PathBuf, anyhow::Error> {
    if program.contains('/') {
        return Ok(PathBuf::from(program));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| anyhow::anyhow!("`{program}` is not on PATH — is it installed?"))
}

/// The name half of a repo slug (`blooop/wayfinder` → `wayfinder`). Display
/// only — never an identity key, because a fork and its upstream share it.
fn short_repo(slug: &str) -> &str {
    slug.split('/').next_back().unwrap_or(slug)
}

/// The checkouts that could host a ticket's agent: every registered checkout of
/// the ticket's repo, matched on the **full** slug (#15). Cache order (sorted
/// by path) is preserved so the picker is stable.
pub fn candidate_checkouts<'a>(checkouts: &'a [Checkout], repo: &str) -> Vec<&'a Checkout> {
    checkouts.iter().filter(|c| c.repo == repo).collect()
}

/// What a launch request resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Targets {
    /// No registered checkout of this repo on this machine — nothing to launch
    /// into. (Only reachable if the cache changed under us: map tickets exist
    /// because a cached checkout named their repo.)
    Unregistered,
    /// Exactly one candidate: launch straight away, no prompt.
    One(Launch),
    /// Several checkouts of one repo (the k1–k5 pattern): the human picks which
    /// one the agent runs in. The only reason the picker still exists — the
    /// agent must run in exactly one tree, and `wf` cannot guess which.
    Many(Vec<Launch>),
}

/// Resolve a launch request against the projects cache. Zero or one candidate
/// never prompts. The aim and mode arrive already settled — this function
/// answers *where* the agent can run, and resolves the route the mode picked.
pub fn plan(checkouts: &[Checkout], staged: &Staged, mode: &LaunchMode) -> Targets {
    let StagedAt::Node { aim, map_issue } = staged.at else {
        // Unreachable from the picker: [`Staged::candidates`] offers no launch
        // row on the map-less door, so nothing there can ask for a node
        // launch. Refusing rather than inventing a node keeps this total.
        return Targets::Unregistered;
    };
    let route = route(&aim, mode.mode());
    resolve(checkouts, &staged.repo, |_| Job::Node {
        aim,
        map_issue,
        route,
        mode: mode.clone(),
    })
}

/// Resolve a creation against the projects cache — the same rules as [`plan`]:
/// zero or one candidate checkout never prompts. The creation arrives already
/// complete ([`CreationKind::with_text`] refused the empty task), so this
/// function only answers *where* the skill runs.
pub fn plan_create(checkouts: &[Checkout], repo: &str, creation: Creation) -> Targets {
    resolve(checkouts, repo, |_| Job::Create(creation.clone()))
}

/// The shared half of [`plan`] and [`plan_create`]: every registered checkout
/// of the repo becomes a complete [`Launch`] carrying the job, and the count
/// decides whether anyone is prompted.
///
/// Isolation is decided **here**, per candidate, rather than at the exec: a
/// checkout that has a devcontainer and one that does not can both be
/// candidates, the notice has to say which is which before the human picks,
/// and by the exec there is nothing left to tell them with. It is the one
/// filesystem read in this module's otherwise pure planning path.
///
/// # Panics
///
/// Never: the `expect` in the one-candidate arm is guarded by the `match` on
/// the length immediately above it.
fn resolve(checkouts: &[Checkout], repo: &str, job: impl Fn(&Checkout) -> Job) -> Targets {
    let launches: Vec<Launch> = candidate_checkouts(checkouts, repo)
        .into_iter()
        .map(|c| Launch {
            repo: repo.to_string(),
            cwd: c.path.clone(),
            job: job(c),
            isolation: Isolation::detect(&c.path),
        })
        .collect();
    match launches.len() {
        0 => Targets::Unregistered,
        1 => Targets::One(launches.into_iter().next().expect("len checked")),
        _ => Targets::Many(launches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Status, TicketType};

    fn ticket(repo: &str, number: u64) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: "the ticket".to_string(),
            status: classify(true, false, vec![]),
            ticket_type: TicketType::Task,
            blocked_by: vec![],
            prs: vec![],
        }
    }

    fn checkout(path: &str, repo: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(path),
            repo: repo.to_string(),
        }
    }

    /// What the picker composes with the default mode row selected, steered by
    /// whatever was typed into it.
    fn interactive(steer: &str) -> LaunchMode {
        LaunchMode::picked(Mode::Interactive, steer)
    }

    /// The same with the `auto` row selected.
    fn auto(steer: &str) -> LaunchMode {
        LaunchMode::picked(Mode::Auto, steer)
    }

    /// The same with the `plain` row selected — the launch that hands the
    /// session no skill at all.
    fn plain(steer: &str) -> LaunchMode {
        LaunchMode::picked(Mode::Plain, steer)
    }

    /// An interactive `/wf` plan — the default launch, and the shape
    /// every checkout-resolution test wants (route and mode are orthogonal to
    /// which trees are candidates).
    fn plan_wf(checkouts: &[Checkout], ticket: &Ticket, map_issue: u64) -> Targets {
        plan(
            checkouts,
            &Staged::ticket(ticket, map_issue, Stage::Ready).expect("ready is launchable"),
            &LaunchMode::picked(Mode::Interactive, ""),
        )
    }

    fn cache() -> Vec<Checkout> {
        vec![
            checkout("/data/k1/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/k2/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/proj/wayfinder", "blooop/wayfinder"),
            checkout("/data/proj/dotfiles", "upstream/dotfiles"),
        ]
    }

    #[test]
    fn candidate_checkouts_filter_on_the_full_slug() {
        let cache = cache();
        let kinisi = candidate_checkouts(&cache, "kinisi/kinisi_ros");
        assert_eq!(
            kinisi.iter().map(|c| c.path.as_path()).collect::<Vec<_>>(),
            vec![
                Path::new("/data/k1/kinisi_ros"),
                Path::new("/data/k2/kinisi_ros")
            ]
        );
        // A fork and its upstream share a short name: matching must not mix
        // them, so "blooop/dotfiles" has no candidate here.
        assert!(candidate_checkouts(&cache, "blooop/dotfiles").is_empty());
        assert_eq!(candidate_checkouts(&cache, "upstream/dotfiles").len(), 1);
    }

    #[test]
    fn one_candidate_never_prompts_and_none_is_unregistered() {
        let cache = cache();
        match plan_wf(&cache, &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(launch) => {
                assert_eq!(launch.cwd(), Path::new("/data/proj/wayfinder"));
                assert_eq!(launch.key(), "wayfinder#16");
                assert!(matches!(
                    &launch.job,
                    Job::Node {
                        map_issue: 1,
                        aim: Aim::Ticket { number: 16, .. },
                        ..
                    }
                ));
            }
            other => panic!("expected One, got {other:?}"),
        }
        assert_eq!(
            plan_wf(&cache, &ticket("blooop/dotfiles", 3), 2),
            Targets::Unregistered
        );
        assert_eq!(
            plan_wf(&[], &ticket("blooop/wayfinder", 16), 1),
            Targets::Unregistered
        );
    }

    #[test]
    fn several_checkouts_of_one_repo_offer_a_choice_of_trees() {
        let launches = match plan_wf(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(launches) => launches,
            other => panic!("expected Many, got {other:?}"),
        };
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches.iter().map(Launch::cwd).collect::<Vec<_>>(),
            vec![
                Path::new("/data/k1/kinisi_ros"),
                Path::new("/data/k2/kinisi_ros")
            ]
        );
        // Same ticket either way: only the tree it runs in differs.
        assert!(launches.iter().all(|l| l.key() == "kinisi_ros#42"));
    }

    #[test]
    fn the_agent_runs_interactive_claude_with_one_prompt_argument() {
        let launch = match plan_wf(&cache(), &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launch.agent_argv(),
            vec![
                "claude".to_string(),
                SKIP_PERMISSIONS.to_string(),
                "/wf 1 16".to_string()
            ]
        );
    }

    #[test]
    fn a_picked_launch_keeps_who_decides_out_of_what_was_typed() {
        // The two axes (#62/#96), now that the mode is a picked row and not a
        // word at the front of one line: the selection decides who resolves the
        // node, and the text only ever steers.
        let picked = |mode: LaunchMode| (mode.mode, mode.steer);
        assert_eq!(picked(interactive("")), (Mode::Interactive, None));
        assert_eq!(picked(auto("")), (Mode::Auto, None));
        // An all-whitespace field is an empty one, not a steer made of spaces.
        assert_eq!(picked(interactive("   ")), (Mode::Interactive, None));
        assert_eq!(picked(auto("  \t ")), (Mode::Auto, None));
        assert_eq!(
            picked(auto("skip the flaky suite")),
            (Mode::Auto, Some("skip the flaky suite".to_string()))
        );
        assert_eq!(
            picked(interactive("try the other approach")),
            (
                Mode::Interactive,
                Some("try the other approach".to_string())
            )
        );
        // The mode words of #62 and #96 are both ordinary steering text now:
        // no string can move the mode the human is looking at, so there is no
        // longer such a thing as a launch that went unattended because of what
        // it happened to start with.
        for typed in ["auto", "auto merge when green", "automate it", "defer"] {
            assert_eq!(
                picked(interactive(typed)),
                (Mode::Interactive, Some(typed.to_string())),
                "{typed:?} steers an interactive launch"
            );
        }
    }

    #[test]
    fn a_ticket_picker_lists_exactly_the_three_launch_modes() {
        // #114: creation is a repo-level act, and a ticket is not a repo-level
        // stop — its picker stays the pure mode list, concerns unmerged.
        let staged =
            Staged::ticket(&ticket("blooop/wayfinder", 16), 1, Stage::Ready).expect("launchable");
        assert_eq!(
            staged.candidates(),
            vec![
                Candidate::Launch {
                    mode: Mode::Interactive,
                    route: Route::Wayfinder
                },
                Candidate::Launch {
                    mode: Mode::Auto,
                    route: Route::WayfinderAuto
                },
                Candidate::Launch {
                    mode: Mode::Plain,
                    route: Route::Plain
                },
            ]
        );
    }
    #[test]
    fn a_header_picker_adds_the_creation_rows_after_the_launch_rows() {
        // The repo-level stop is where creation lives: the same three launch
        // rows, then the three ways to start something new in this repo. Each
        // candidate is complete — it carries its own resolved route, the
        // `Targets::Many` move — so a row and its launch cannot disagree.
        let staged = Staged::map(&MapId::new("blooop/wayfinder", 59), "the dev-process tree");
        let candidates = staged.candidates();
        assert_eq!(
            candidates,
            vec![
                Candidate::Launch {
                    mode: Mode::Interactive,
                    route: Route::Wayfinder
                },
                Candidate::Launch {
                    mode: Mode::Auto,
                    route: Route::WayfinderAuto
                },
                Candidate::Launch {
                    mode: Mode::Plain,
                    route: Route::Plain
                },
                Candidate::Create(CreationKind::Task),
                Candidate::Create(CreationKind::Map),
                Candidate::Create(CreationKind::MapAuto),
            ]
        );
        // Every row names the skill it execs — including the creation rows,
        // whose routes are theirs rather than the staged node's.
        let routes: Vec<Route> = candidates.iter().map(|c| c.route()).collect();
        assert_eq!(
            routes[3..],
            [Route::One, Route::Wayfinder, Route::WayfinderAuto]
        );
    }

    #[test]
    fn a_build_tickets_launch_rows_resolve_its_own_stage_routes() {
        // Complete candidates mean the per-row route is the (aim, mode) answer
        // for *this* node: a build ticket's interactive row reads /wf-tdd, not
        // a generic default.
        let mut node = ticket("blooop/wayfinder", 16);
        node.ticket_type = TicketType::Build;
        let staged = Staged::ticket(&node, 1, Stage::Ready).expect("launchable");
        assert_eq!(
            staged.candidates()[0],
            Candidate::Launch {
                mode: Mode::Interactive,
                route: Route::Tdd
            }
        );
    }

    #[test]
    fn creation_rows_read_as_what_they_start() {
        assert_eq!(Candidate::Create(CreationKind::Task).label(), "new task");
        assert_eq!(Candidate::Create(CreationKind::Map).label(), "new map");
        assert_eq!(
            Candidate::Create(CreationKind::MapAuto).label(),
            "new map, auto"
        );
        // Launch rows keep reading as their mode.
        assert_eq!(
            Candidate::Launch {
                mode: Mode::Interactive,
                route: Route::Wayfinder
            }
            .label(),
            "interactive"
        );
        // The text field names what typing into it means, per row: steering an
        // agent, the task itself, or a seed for the charting session.
        assert_eq!(
            Candidate::Launch {
                mode: Mode::Auto,
                route: Route::WayfinderAuto
            }
            .field(),
            "steer"
        );
        assert_eq!(Candidate::Create(CreationKind::Task).field(), "task");
        assert_eq!(Candidate::Create(CreationKind::Map).field(), "seed");
        assert_eq!(Candidate::Create(CreationKind::MapAuto).field(), "seed");
    }

    #[test]
    fn a_task_needs_its_text_and_a_map_seed_is_optional() {
        // Parse, don't validate (#114): an empty task is refused where the
        // creation is built, so a `/wf-one` with nothing to do is
        // unrepresentable — and all-whitespace is empty, as the steer field
        // already treats it. A map's text is only a seed: the charting session
        // grills for the idea anyway, so nothing typed is a fine seed.
        assert_eq!(CreationKind::Task.with_text("   "), None);
        assert_eq!(
            CreationKind::Task.with_text(" wire the exporter "),
            Some(Creation::Task {
                task: "wire the exporter".to_string()
            })
        );
        assert_eq!(
            CreationKind::Map.with_text(""),
            Some(Creation::Map { seed: None })
        );
        assert_eq!(
            CreationKind::Map.with_text(" a caching layer "),
            Some(Creation::Map {
                seed: Some("a caching layer".to_string())
            })
        );
        assert_eq!(
            CreationKind::MapAuto.with_text(""),
            Some(Creation::MapAuto { seed: None })
        );
    }

    /// The one-checkout creation launch, reduced to its argv.
    fn creation_argv(kind: CreationKind, text: &str) -> Vec<String> {
        let creation = kind.with_text(text).expect("a buildable creation");
        match plan_create(&cache(), "blooop/wayfinder", creation) {
            Targets::One(l) => l.agent_argv(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_new_task_launch_execs_wf_one_with_the_task_verbatim() {
        // The typed text *is* the ticket — no `steer:` prefix, no skill to
        // steer: `/wf-one` receives the task as its argument.
        assert_eq!(
            creation_argv(CreationKind::Task, "wire the exporter"),
            vec![
                "claude".to_string(),
                SKIP_PERMISSIONS.to_string(),
                "/wf-one wire the exporter".to_string()
            ]
        );
    }

    #[test]
    fn a_new_map_launch_execs_the_charting_skill_with_an_optional_seed() {
        // Bare `/wf` charts from nothing; typed text rides as the loose idea.
        assert_eq!(
            creation_argv(CreationKind::Map, "").last().expect("prompt"),
            "/wf"
        );
        assert_eq!(
            creation_argv(CreationKind::Map, "a caching layer")
                .last()
                .expect("prompt"),
            "/wf a caching layer"
        );
        assert_eq!(
            creation_argv(CreationKind::MapAuto, "")
                .last()
                .expect("prompt"),
            "/wf-auto"
        );
        assert_eq!(
            creation_argv(CreationKind::MapAuto, "a caching layer")
                .last()
                .expect("prompt"),
            "/wf-auto a caching layer"
        );
    }

    #[test]
    fn an_isolated_creation_launch_runs_on_the_default_workspace() {
        // A creation has no issue number, so no per-ticket branch exists to
        // name: `dl` gets the bare `owner/repo` spec — the default workspace —
        // and the launched skill files its own issues and makes its own
        // branches (#114). The command after `--` is quoted like any other.
        let launch = isolated_creation(
            CreationKind::Task
                .with_text("wire the exporter")
                .expect("text given"),
        );
        assert_eq!(
            launch.agent_argv(),
            vec![
                "dl".to_string(),
                "blooop/wayfinder".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions' '/wf-one wire the exporter'".to_string(),
            ]
        );
    }

    #[test]
    fn the_notice_names_what_a_creation_starts_and_where() {
        // The launch notice is the last thing wf says: for a creation there is
        // no `#n` to name, so it names the act.
        let creation = CreationKind::Map.with_text("").expect("seedless map");
        match plan_create(&cache(), "blooop/wayfinder", creation) {
            Targets::One(l) => {
                assert_eq!(l.describe(), "new map in /data/proj/wayfinder");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            isolated_creation(CreationKind::Task.with_text("x").expect("text")).describe(),
            "new task in blooop/wayfinder (devlaunch)"
        );
    }

    #[test]
    fn a_creation_resolves_checkouts_like_any_other_launch() {
        // Same cache, same rules: none registered refuses, several prompt.
        let creation = || CreationKind::Map.with_text("").expect("seedless map");
        assert_eq!(
            plan_create(&cache(), "blooop/dotfiles", creation()),
            Targets::Unregistered
        );
        match plan_create(&cache(), "kinisi/kinisi_ros", creation()) {
            Targets::Many(launches) => assert_eq!(launches.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_new_task_route_names_the_wf_one_skill() {
        // #114: `wf` ships `/wf-one` but routed nothing to it — the one skill
        // in the bundle with no arm in `route`. The creation candidates give
        // it its arm, so it must exist as a route and name its bundled skill.
        assert_eq!(Route::One.label(), "/wf-one");
        assert_eq!(Route::One.bundled_skill(), Some("wf-one"));
        assert!(Route::all().contains(&Route::One), "reachable in the cycle");
    }

    #[test]
    fn every_mode_is_in_the_picker_and_the_default_leads_it() {
        // The picker lists `Mode::all`, so a mode missing from it would be a
        // mode nothing on screen can reach.
        assert_eq!(
            Mode::all(),
            vec![Mode::Interactive, Mode::Auto, Mode::Plain]
        );
        assert_eq!(
            Mode::all().first(),
            Some(&Mode::default()),
            "enter opens on the default"
        );
        // Derived from the `after` cycle: every mode appears exactly once.
        let mut seen = Mode::all();
        seen.dedup();
        assert_eq!(seen.len(), Mode::all().len());
    }

    /// The whole argv a node of this (type, stage) is launched with, under
    /// `mode`. Whole rather than its last entry, because a plain session's
    /// argv may have no prompt entry to take the last of.
    fn ticket_argv(ticket_type: TicketType, stage: Stage, mode: &LaunchMode) -> Vec<String> {
        let mut node = ticket("blooop/wayfinder", 16);
        node.ticket_type = ticket_type;
        let staged = Staged::ticket(&node, 1, stage).expect("a launchable stage");
        match plan(&cache(), &staged, mode) {
            Targets::One(l) => l.agent_argv(),
            other => panic!("{other:?}"),
        }
    }

    /// The prompt of that launch — for the tests about *what is said* to a
    /// skill, where the argv's shape is not the question.
    fn ticket_prompt(ticket_type: TicketType, stage: Stage, mode: &LaunchMode) -> String {
        ticket_argv(ticket_type, stage, mode)
            .last()
            .expect("a prompt")
            .clone()
    }

    /// The whole argv of a launch aimed at the whole map.
    fn map_argv(mode: &LaunchMode) -> Vec<String> {
        let staged = Staged::map(&MapId::new("blooop/wayfinder", 59), "the dev-process tree");
        match plan(&cache(), &staged, mode) {
            Targets::One(l) => l.agent_argv(),
            other => panic!("{other:?}"),
        }
    }

    /// The same, reduced to the prompt.
    fn map_prompt(mode: &LaunchMode) -> String {
        map_argv(mode).last().expect("a prompt").clone()
    }

    #[test]
    fn the_agent_command_is_the_route_plus_the_steering_suffix() {
        // The route picks the skill; only the wayfinder skills take the map
        // argument. The mode is *not* in the suffix — it chose the skill.
        assert_eq!(
            ticket_prompt(TicketType::Build, Stage::Ready, &interactive("")),
            "/wf-tdd 16"
        );
        assert_eq!(
            ticket_prompt(TicketType::Build, Stage::InReview, &interactive("")),
            "/wf-review 16"
        );
        assert_eq!(
            ticket_prompt(TicketType::Grilling, Stage::Ready, &interactive("")),
            "/wf 1 16"
        );
        assert_eq!(
            ticket_prompt(TicketType::Grilling, Stage::Ready, &auto("")),
            "/wf-auto 1 16"
        );
        // Steering rides as a suffix, whatever the route.
        assert_eq!(
            ticket_prompt(
                TicketType::Grilling,
                Stage::Ready,
                &auto("skip the flaky suite")
            ),
            "/wf-auto 1 16 steer: skip the flaky suite"
        );
        assert_eq!(
            ticket_prompt(
                TicketType::Build,
                Stage::Ready,
                &interactive("try the other approach")
            ),
            "/wf-tdd 16 steer: try the other approach"
        );
    }

    #[test]
    fn a_map_launch_is_the_skill_and_the_map_number_alone() {
        // No ticket argument exists to pass, so none is passed — the map aim
        // is the whole subject (#96).
        assert_eq!(map_prompt(&interactive("")), "/wf 59");
        assert_eq!(map_prompt(&auto("")), "/wf-auto 59");
        assert_eq!(
            map_prompt(&auto("merge when green")),
            "/wf-auto 59 steer: merge when green"
        );
        // A map's key is its own issue number, not a ticket's.
        let staged = Staged::map(&MapId::new("blooop/wayfinder", 59), "the dev-process tree");
        assert_eq!(staged.key(), "#59");
        match plan(&cache(), &staged, &interactive("")) {
            Targets::One(l) => assert_eq!(l.key(), "wayfinder#59"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_notice_names_the_ticket_and_the_tree_it_runs_in() {
        // With several checkouts, *which tree* is the only thing that varies —
        // so it is what the notice has to say.
        let launches = match plan_wf(&cache(), &ticket("kinisi/kinisi_ros", 42), 7) {
            Targets::Many(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launches[0].describe(),
            "kinisi_ros#42 in /data/k1/kinisi_ros"
        );
        assert_eq!(
            launches[1].describe(),
            "kinisi_ros#42 in /data/k2/kinisi_ros"
        );
    }

    #[test]
    fn the_key_is_the_short_repo_but_identity_stays_the_full_slug() {
        let launch = match plan_wf(&cache(), &ticket("upstream/dotfiles", 5), 4) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(launch.key(), "dotfiles#5");
        assert_eq!(launch.repo, "upstream/dotfiles");
        assert_eq!(short_repo("blooop/wayfinder"), "wayfinder");
        // Not a slug at all: the whole thing is the name.
        assert_eq!(short_repo("wayfinder"), "wayfinder");
    }

    /// Every decision type, and every stage a launch can act on — the two
    /// axes the routing table is swept across.
    const DECISION_TYPES: [TicketType; 5] = [
        TicketType::Research,
        TicketType::Task,
        TicketType::Grilling,
        TicketType::Prototype,
        TicketType::Untyped,
    ];
    const LAUNCHABLE: [Launchable; 4] = [
        Launchable::Ready,
        Launchable::Building,
        Launchable::InReview,
        Launchable::NeedsAttention,
    ];

    /// A ticket aim, for asking [`route`] about one cell of the table.
    fn aim(ticket_type: TicketType, stage: Launchable) -> Aim {
        Aim::Ticket {
            number: 16,
            ticket_type,
            stage,
        }
    }

    #[test]
    fn build_nodes_route_to_tdd_except_in_review_which_routes_to_review() {
        // The #61 routing table's build rows: failing checks and requested
        // changes are code work, so needs-attention goes back to /wf-tdd.
        let build = |stage| route(&aim(TicketType::Build, stage), Mode::Interactive);
        assert_eq!(build(Launchable::Ready), Route::Tdd);
        assert_eq!(build(Launchable::Building), Route::Tdd);
        assert_eq!(build(Launchable::NeedsAttention), Route::Tdd);
        assert_eq!(build(Launchable::InReview), Route::Review);
    }

    #[test]
    fn decision_types_route_to_wayfinder_at_every_unfinished_stage() {
        // The table lists decision types at ready/in-progress, but PR-dominant
        // derivation can put one at in-review or needs-attention (a
        // prototype's PR counts) — the skill owns its node's PR state, so the
        // route stays /wf at every stage short of done. Untyped rides
        // along: launching untyped tickets is today's behavior, kept.
        for ticket_type in DECISION_TYPES {
            for stage in LAUNCHABLE {
                assert_eq!(
                    route(&aim(ticket_type, stage), Mode::Interactive),
                    Route::Wayfinder,
                    "{ticket_type:?} at {stage:?}"
                );
            }
        }
    }

    #[test]
    fn auto_routes_every_node_to_wayfinder_auto() {
        // The mode axis collapses the table (#96): under `auto` the launched
        // session manages the node's whole remaining lifecycle, so it is the
        // manager skill that runs — never the stage's own skill, which would
        // do one stage and stop.
        for ticket_type in DECISION_TYPES.into_iter().chain([TicketType::Build]) {
            for stage in LAUNCHABLE {
                assert_eq!(
                    route(&aim(ticket_type, stage), Mode::Auto),
                    Route::WayfinderAuto,
                    "{ticket_type:?} at {stage:?}"
                );
            }
        }
        assert_eq!(route(&Aim::Map, Mode::Auto), Route::WayfinderAuto);
        assert_eq!(route(&Aim::Map, Mode::Interactive), Route::Wayfinder);
    }

    #[test]
    fn plain_launches_a_session_with_no_skill_in_it() {
        // The third mode collapses the table the way `auto` does, and for the
        // opposite reason: `auto` picks one skill for every node, `plain` picks
        // none. Which node it was aimed at cannot change that, so every cell
        // answers the same.
        for ticket_type in DECISION_TYPES.into_iter().chain([TicketType::Build]) {
            for stage in LAUNCHABLE {
                assert_eq!(
                    route(&aim(ticket_type, stage), Mode::Plain),
                    Route::Plain,
                    "{ticket_type:?} at {stage:?}"
                );
            }
        }
        assert_eq!(route(&Aim::Map, Mode::Plain), Route::Plain);
        // And the exec is `claude` with nothing said to it: no slash command,
        // and no prompt argument at all rather than an empty one.
        assert_eq!(
            ticket_argv(TicketType::Build, Stage::Ready, &plain("")),
            vec!["claude".to_string(), SKIP_PERMISSIONS.to_string()]
        );
    }

    #[test]
    fn a_plain_session_opens_on_what_was_typed_and_nothing_else() {
        // Steering rides as ` steer: …` because there is a skill in front of
        // it to steer. With no skill the same suffix would be addressed to
        // nobody, so the text is simply the session's first message — and the
        // map number, which is an argument to a skill and not to `claude`,
        // does not appear either.
        assert_eq!(
            ticket_argv(TicketType::Build, Stage::Ready, &plain("rebase onto main")),
            vec![
                "claude".to_string(),
                SKIP_PERMISSIONS.to_string(),
                "rebase onto main".to_string()
            ]
        );
        assert_eq!(
            map_argv(&plain("what is actually left in here?")),
            vec![
                "claude".to_string(),
                SKIP_PERMISSIONS.to_string(),
                "what is actually left in here?".to_string()
            ]
        );
        assert_eq!(
            map_argv(&plain("")),
            vec!["claude".to_string(), SKIP_PERMISSIONS.to_string()]
        );
    }

    #[test]
    fn done_is_not_launchable_whatever_the_type() {
        // The refusal moved off `route` and onto the parse that builds the
        // aim, so it is made once and cannot be forgotten by a caller.
        assert_eq!(Launchable::parse(Stage::Done), None);
        for ticket_type in DECISION_TYPES.into_iter().chain([TicketType::Build]) {
            let mut node = ticket("blooop/wayfinder", 16);
            node.ticket_type = ticket_type;
            assert_eq!(
                Staged::ticket(&node, 1, Stage::Done),
                None,
                "{ticket_type:?}"
            );
        }
    }

    /// A scratch directory of our own, removed by the test that made it. No
    /// `tempfile` dependency for three tests that need a path to exist.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("wf-isolation-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        /// Create `rel` (and its parents) as an empty file.
        fn touch(&self, rel: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("parents");
            std::fs::write(&path, "").expect("touch");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_checkout_declares_isolation_by_carrying_a_default_devcontainer_config() {
        // Both spec locations count, and only files — a `.devcontainer/`
        // directory with no config in it is not a declaration.
        let bare = Scratch::new("bare");
        assert!(!has_devcontainer(&bare.0));
        bare.touch(".devcontainer/README.md");
        assert!(!has_devcontainer(&bare.0));

        let nested = Scratch::new("nested");
        nested.touch(".devcontainer/devcontainer.json");
        assert!(has_devcontainer(&nested.0));

        let top = Scratch::new("top");
        top.touch(".devcontainer.json");
        assert!(has_devcontainer(&top.0));

        // A variant-only layout is deliberately not a declaration: `wf` would
        // have to choose the variant, and it has no basis to.
        let variants = Scratch::new("variants");
        variants.touch(".devcontainer/gpu/devcontainer.json");
        assert!(!has_devcontainer(&variants.0));
    }

    #[test]
    fn a_checkout_without_a_devcontainer_runs_on_the_host_whatever_is_on_path() {
        // The other half — a devcontainer but no `dl` — is the degradation
        // path, and it is what every other test in this module exercises by
        // running on a machine that may or may not have `dl`. This direction is
        // the one that must hold unconditionally.
        let bare = Scratch::new("host");
        assert_eq!(Isolation::detect(&bare.0), Isolation::Host);
        assert_eq!(
            plan_wf(&cache(), &ticket("blooop/wayfinder", 16), 1),
            plan_wf(&cache(), &ticket("blooop/wayfinder", 16), 1)
        );
        // The cache's paths do not exist, so nothing in this module's planning
        // tests can be anything but Host.
        match plan_wf(&cache(), &ticket("blooop/wayfinder", 16), 1) {
            Targets::One(launch) => assert_eq!(launch.isolation(), Isolation::Host),
            other => panic!("{other:?}"),
        }
    }

    /// The same launch, forced into a container — the fields are private and
    /// `plan` reads the real filesystem, so a test that wants the isolated
    /// shape builds it here rather than arranging a `dl` on PATH.
    fn isolated(route: Route, mode: LaunchMode) -> Launch {
        isolated_ticket(80, route, mode)
    }

    fn isolated_ticket(number: u64, route: Route, mode: LaunchMode) -> Launch {
        Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Node {
                aim: Aim::Ticket {
                    number,
                    ticket_type: TicketType::Task,
                    stage: Launchable::Ready,
                },
                map_issue: 67,
                route,
                mode,
            },
            isolation: Isolation::Devlaunch,
        }
    }

    /// A creation launch forced into a container, for the same reason as
    /// [`isolated`]: `plan_create` reads the real filesystem.
    fn isolated_creation(creation: Creation) -> Launch {
        Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Create(creation),
            isolation: Isolation::Devlaunch,
        }
    }

    #[test]
    fn an_isolated_launch_hands_dl_a_per_node_workspace_and_a_quoted_shell_command() {
        // The workspace is `owner/repo@wayfinder/<repo>-<n>`: `dl` clones that
        // branch into a tree and container of the node's own, so the human's
        // checkout is never the agent's working tree. `dl` joins everything
        // after `--` and runs it through a shell in the container, so the
        // prompt has to arrive already quoted or it lands as three arguments;
        // the workspace spec is a plain argv entry and is not quoted.
        assert_eq!(
            isolated(Route::Wayfinder, interactive("")).agent_argv(),
            vec![
                "dl".to_string(),
                "blooop/wayfinder@wayfinder/wayfinder-80".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions' '/wf 67 80'".to_string(),
            ]
        );
        // The steering suffix rides inside the same quoted argument.
        assert_eq!(
            isolated(Route::WayfinderAuto, auto("merge when green")).agent_argv()[3],
            "'claude' '--dangerously-skip-permissions' '/wf-auto 67 80 steer: merge when green'"
        );
    }

    #[test]
    fn an_isolated_plain_session_gets_the_nodes_workspace_like_any_other_launch() {
        // The point of the mode: the branch, the clone and the container are
        // exactly what a skill launch would have got — `wf` still did all of
        // that — and the only difference is that nothing is invoked inside it.
        assert_eq!(
            isolated(Route::Plain, plain("")).agent_argv(),
            vec![
                "dl".to_string(),
                "blooop/wayfinder@wayfinder/wayfinder-80".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions'".to_string(),
            ]
        );
        // The typed prompt is one quoted argument here too, so a sentence
        // arrives as a sentence rather than as several arguments.
        assert_eq!(
            isolated(Route::Plain, plain("check what the logs say")).agent_argv()[3],
            "'claude' '--dangerously-skip-permissions' 'check what the logs say'"
        );
    }

    #[test]
    fn parallel_nodes_get_distinct_workspaces_and_a_relaunch_gets_its_own_back() {
        // The reason the seam is a branch and not the checkout path: two
        // tickets launched at once must not share a tree or a container, and
        // the same ticket launched twice must land in the same workspace
        // (that is `dl` reattaching, not a second clone).
        let a = isolated_ticket(80, Route::Tdd, interactive("")).agent_argv()[1].clone();
        let b = isolated_ticket(81, Route::Tdd, interactive("")).agent_argv()[1].clone();
        let a_again = isolated_ticket(80, Route::Review, interactive("")).agent_argv()[1].clone();
        assert_ne!(a, b);
        assert_eq!(a, a_again);
        // The branch is the one `/wf-tdd` is told to work on, so a build agent
        // wakes up already on its work branch.
        assert_eq!(a, "blooop/wayfinder@wayfinder/wayfinder-80");
    }

    #[test]
    fn a_map_launch_gets_a_workspace_named_by_the_map_issue() {
        // A whole-map session is a node like any other: its own branch, its
        // own container, keyed by the map issue since there is no ticket.
        let launch = Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Node {
                aim: Aim::Map,
                map_issue: 67,
                route: Route::WayfinderAuto,
                mode: auto(""),
            },
            isolation: Isolation::Devlaunch,
        };
        assert_eq!(
            launch.agent_argv(),
            vec![
                "dl".to_string(),
                "blooop/wayfinder@wayfinder/wayfinder-67".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions' '/wf-auto 67'".to_string(),
            ]
        );
    }

    #[test]
    fn steering_text_cannot_break_out_of_the_shell_command() {
        // The one string a user types that reaches a shell. A single quote
        // closes, escapes, reopens — the argument stays one argument.
        let launch = isolated(Route::Tdd, interactive("don't touch the CI; rm -rf /"));
        assert_eq!(
            launch.agent_argv()[3],
            r"'claude' '--dangerously-skip-permissions' '/wf-tdd 80 steer: don'\''t touch the CI; rm -rf /'"
        );
        // Every metacharacter a shell would otherwise act on is inside quotes.
        assert_eq!(shell_quote("a b|c;d&e$f`g"), "'a b|c;d&e$f`g'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn the_notice_names_the_container_when_there_is_one_and_stays_quiet_otherwise() {
        // An isolated launch works in its workspace, not in the checkout —
        // so the workspace is what the notice names.
        assert_eq!(
            isolated(Route::Wayfinder, interactive("")).describe(),
            "wayfinder#80 in blooop/wayfinder@wayfinder/wayfinder-80 (devlaunch)"
        );
        match plan_wf(&cache(), &ticket("blooop/wayfinder", 80), 67) {
            Targets::One(launch) => {
                assert_eq!(launch.describe(), "wayfinder#80 in /data/proj/wayfinder");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn status_is_irrelevant_to_launching() {
        let done = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 2,
            title: "done".to_string(),
            status: Status::Done,
            ticket_type: TicketType::Task,
            blocked_by: vec![],
            prs: vec![],
        };
        match plan_wf(&cache(), &done, 1) {
            Targets::One(launch) => assert_eq!(launch.key(), "wayfinder#2"),
            other => panic!("a done ticket still launches, got {other:?}"),
        }
    }
}
