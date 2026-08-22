//! The launch: a picked ticket becomes the agent, running right here.
//!
//! `wf` is a selector (#26/#34). There is no multiplexer, no tab, no session
//! and no supervision: the picked node resolves to a checkout, `wf` gives the
//! terminal back, and its own process image is replaced by the selected agent
//! in that checkout ([`Launch::exec`]) — which skill is [`route`]'s answer to
//! what was picked ([`Aim`]) and who decides ([`Mode`]), and any steering text
//! rides the prompt as a suffix (#61/#62/#96). Unattended work is still not
//! supervised here — an `auto` launch is the same exec of the `wf-auto` skill,
//! watched from another terminal or not at all. One mode invokes no skill at
//! all ([`Mode::Plain`], #112): the same exec, in the same workspace, with the
//! prompt left to the human.
//!
//! A checkout that declares a devcontainer runs a **Claude** launch *inside* a
//! container, by way of `dl` ([`Isolation`], #80): `wf` owns which ticket,
//! which checkout, which skill and which prompt, and `dl` owns the container,
//! its lifecycle and its credentials. The seam is a **per-node workspace**,
//! `owner/repo@wayfinder/<repo>-<n>` ([`Launch::agent_argv`]): every launched
//! node gets its own branch, its own clone under `dl`'s cache and its own
//! container, so any number of tickets run at once without colliding — and
//! the tree the human picked in the checkout picker stays theirs, never
//! mutated by an agent. Codex stays on the host until `dl` can carry its
//! configuration too. (#80 originally handed `dl` the checkout path so the
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
use std::sync::OnceLock;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::model::{MapId, PrLink, Stage, Ticket, TicketType};
use crate::projects::{Checkout, Resume, Session};

/// Which interactive coding agent `wf` becomes after a launch.
///
/// The bundle is deliberately shared: both CLIs read Agent Skills directories,
/// and a route is the same named workflow whichever one runs it. What differs
/// is the mention syntax (`/wf` for Claude Code, `$wf` for Codex) and the
/// command-line permission switch. Keeping that translation here means the
/// picker cannot display one agent while the exec path silently starts the
/// other.
/// It is also the one half of a [`Resume`] that
/// cannot be re-derived, so it goes to disk with one — spelt in lower case,
/// which is what a human editing that cache would write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    /// Claude Code, which is the longstanding `wf` default.
    #[default]
    Claude,
    /// Codex CLI, which names installed skills with `$` mentions.
    Codex,
}

impl Agent {
    /// The name shown in the launch picker's title.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    /// The CLI program this choice starts.
    fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// The explicit skill sigil each CLI recognises in an initial prompt.
    fn skill_sigil(self) -> char {
        match self {
            Agent::Claude => '/',
            Agent::Codex => '$',
        }
    }

    /// How this CLI is told to rejoin the conversation in its working
    /// directory, as argv between the program and its permission switch.
    ///
    /// Both are **cwd-scoped**, which is the fact the whole resume rests on:
    /// `claude --continue` continues "the most recent conversation in the
    /// current directory", and `codex resume` filters by cwd unless `--all`
    /// says otherwise. `wf` already gives every node a working directory of
    /// its own — the per-node workspace — so rejoining is a matter of exec'ing
    /// in the same place, and no session id ever has to be stored, matched or
    /// go stale.
    ///
    /// Codex spells it as a subcommand, which is why this returns a slice
    /// rather than a flag: the bypass switch and the prompt both have to come
    /// after it.
    fn resume_argv(self) -> &'static [&'static str] {
        match self {
            Agent::Claude => &["--continue"],
            Agent::Codex => &["resume", "--last"],
        }
    }

    /// The permission bypass matching an interactive agent launched from this
    /// picker. `wf` has always handed Claude its equivalent: the picker is the
    /// deliberate opt-in point, and neither agent should stop after `wf` has
    /// restored the terminal and replaced itself.
    fn skip_permissions(self) -> &'static str {
        match self {
            Agent::Claude => "--dangerously-skip-permissions",
            Agent::Codex => "--dangerously-bypass-approvals-and-sandbox",
        }
    }

    /// The other agent. The picker has two choices, so each horizontal arrow
    /// is both previous and next; naming the operation keeps that fact local.
    #[must_use]
    pub fn other(self) -> Agent {
        match self {
            Agent::Claude => Agent::Codex,
            Agent::Codex => Agent::Claude,
        }
    }

    /// Every launch agent, in picker order, beginning with the compatible
    /// default. This also gives installation one list to follow.
    pub const fn all() -> [Agent; 2] {
        [Agent::Claude, Agent::Codex]
    }
}

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
    /// `/wf-mid <map> [<n>]` — the same map with the human in it for the few
    /// decisions that are genuinely theirs: the skill settles whatever its
    /// principles settle and escalates only taste, scope, and what is
    /// expensive to reverse. A third skill rather than a flag on either
    /// neighbour, for the reason [`Mode`] gives: who decides is the whole
    /// difference between these prompts, so it is spent at the exec.
    WayfinderMid,
    /// `/wf-one <task>` — one tracked ticket, filed and driven by the skill:
    /// its own single-ticket map, `/wf-tdd` build, `/wf-review` review (#114).
    /// The only route reached by a creation candidate rather than a mode.
    One,
    /// `claude` — no skill at all (#112). The only route that invokes nothing:
    /// the session opens in the node's workspace and the human drives it.
    Plain,
}

impl Route {
    /// The bundled skill this route invokes, without the selected agent's
    /// sigil. [`Route::Plain`] invokes no skill.
    pub fn bundled_skill(self) -> Option<&'static str> {
        match self {
            Route::Tdd => Some("wf-tdd"),
            Route::Review => Some("wf-review"),
            Route::Wayfinder => Some("wf"),
            Route::WayfinderAuto => Some("wf-auto"),
            Route::WayfinderMid => Some("wf-mid"),
            Route::One => Some("wf-one"),
            Route::Plain => None,
        }
    }

    /// How the route is invoked by `agent`, or the agent's own name when no
    /// skill is run.
    pub fn invocation(self, agent: Agent) -> String {
        self.bundled_skill().map_or_else(
            || agent.program().to_string(),
            |skill| format!("{}{skill}", agent.skill_sigil()),
        )
    }

    /// How Claude Code spells this route. Launch prompts use
    /// [`Route::invocation`] so the selected agent controls the sigil.
    pub fn label(self) -> &'static str {
        match self {
            Route::Tdd => "/wf-tdd",
            Route::Review => "/wf-review",
            Route::Wayfinder => "/wf",
            Route::WayfinderAuto => "/wf-auto",
            Route::WayfinderMid => "/wf-mid",
            Route::One => "/wf-one",
            Route::Plain => "claude",
        }
    }

    /// The next route, wrapping, so [`Route::all`] has one source of truth.
    fn after(self) -> Route {
        match self {
            Route::Tdd => Route::Review,
            Route::Review => Route::Wayfinder,
            Route::Wayfinder => Route::WayfinderMid,
            Route::WayfinderMid => Route::WayfinderAuto,
            Route::WayfinderAuto => Route::One,
            Route::One => Route::Plain,
            Route::Plain => Route::Tdd,
        }
    }

    /// Every route in derived picker order.
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
    /// The agent settles what `/wf-mid`'s principles settle and asks about
    /// the rest — the few decisions that are taste, scope, or expensive to
    /// reverse. Sits between the two neighbours on the one axis this picker
    /// is about: `interactive` spends the human's attention on every decision,
    /// `auto` spends none of it, and neither is the right price for a map
    /// whose decisions are mostly obvious and occasionally not.
    Mid,
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
            Mode::Mid => "mid",
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
            Mode::Mid => "it decides what it can, asks what it can't",
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
            Mode::Interactive => Mode::Mid,
            Mode::Mid => Mode::Auto,
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
/// Hand-listed kinds rather than a creation × [`Mode`] product, because the
/// product is dishonest: charting a map with no skill (`plain`) is meaningless,
/// and a task's lifecycle is `/wf-one`'s own. These are exactly the
/// combinations that mean something — the three charting modes plus the task,
/// and no cell for the one that would mean nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationKind {
    /// One tracked ticket via `/wf-one`: filed, built, reviewed.
    Task,
    /// Chart a new map via `/wf`, with the human in the loop.
    Map,
    /// Chart a new map via `/wf-mid`, mostly alone: it drafts the destination
    /// and confirms it once, which is the highest-value question on any map.
    MapMid,
    /// Chart a new map via `/wf-auto`, alone.
    MapAuto,
}

impl CreationKind {
    /// Every kind, in picker order. Written out rather than derived from an
    /// `after` cycle: four variants with one call site is below the size
    /// where the cycle device earns its ceremony.
    pub fn all() -> Vec<CreationKind> {
        vec![
            CreationKind::Task,
            CreationKind::Map,
            CreationKind::MapMid,
            CreationKind::MapAuto,
        ]
    }

    /// How the row reads in the picker.
    pub fn label(self) -> &'static str {
        match self {
            CreationKind::Task => "new task",
            CreationKind::Map => "new map",
            CreationKind::MapMid => "new map, mid",
            CreationKind::MapAuto => "new map, auto",
        }
    }

    /// What picking it means — same register as [`Mode::blurb`].
    pub fn blurb(self) -> &'static str {
        match self {
            CreationKind::Task => "one tracked ticket, built and reviewed",
            CreationKind::Map => "chart a new map in this repo, with you",
            CreationKind::MapMid => "chart a new map in this repo, asking little",
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
            CreationKind::MapMid => Route::WayfinderMid,
            CreationKind::MapAuto => Route::WayfinderAuto,
        }
    }

    /// What the text field means on this row — the name drawn beside it.
    pub fn field(self) -> &'static str {
        match self {
            CreationKind::Task => "task",
            CreationKind::Map | CreationKind::MapMid | CreationKind::MapAuto => "seed",
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
            CreationKind::MapMid => Some(Creation::MapMid { seed: seed() }),
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
    /// `/wf-mid [<seed>]` — charting mostly alone.
    MapMid { seed: Option<String> },
    /// `/wf-auto [<seed>]` — charting alone.
    MapAuto { seed: Option<String> },
}

impl Creation {
    /// The kind this creation completes — for the labels the notice reuses.
    fn kind(&self) -> CreationKind {
        match self {
            Creation::Task { .. } => CreationKind::Task,
            Creation::Map { .. } => CreationKind::Map,
            Creation::MapMid { .. } => CreationKind::MapMid,
            Creation::MapAuto { .. } => CreationKind::MapAuto,
        }
    }

    /// The whole prompt this creation execs. Built here rather than through
    /// [`LaunchMode::opening_prompt`] because nothing is being steered: the
    /// text is the skill's *argument* — the task, or the loose idea — not a
    /// ` steer: …` suffix on a session that already has a subject.
    fn invocation(&self, agent: Agent) -> String {
        let seeded = |skill: &str, seed: &Option<String>| match seed {
            None => skill.to_string(),
            Some(seed) => format!("{skill} {seed}"),
        };
        match self {
            Creation::Task { task } => format!("{} {task}", Route::One.invocation(agent)),
            Creation::Map { seed } => seeded(&Route::Wayfinder.invocation(agent), seed),
            Creation::MapMid { seed } => seeded(&Route::WayfinderMid.invocation(agent), seed),
            Creation::MapAuto { seed } => seeded(&Route::WayfinderAuto.invocation(agent), seed),
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
    /// Rejoin the conversation a previous launch of this node left on this
    /// machine (#35) — no skill, no context block, no fresh session.
    ///
    /// Carries the agent from the record rather than taking the picker's,
    /// because a Claude conversation is not rejoinable by Codex: the row is
    /// only offered for the CLI that actually ran, and that is the CLI it
    /// runs. Which is also why the picker's `←/→` does nothing while it is
    /// picked — see [`Candidate::agent`].
    Resume { agent: Agent },
}

impl Candidate {
    /// How the row reads in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Candidate::Launch { mode, .. } => mode.label(),
            Candidate::Create(kind) => kind.label(),
            Candidate::Resume { .. } => "resume",
        }
    }

    /// What picking it means.
    pub fn blurb(self) -> &'static str {
        match self {
            Candidate::Launch { mode, .. } => mode.blurb(),
            Candidate::Create(kind) => kind.blurb(),
            // Says what it does, not how long ago: the age is a fact about
            // *this* record and is drawn from it, beside the row.
            Candidate::Resume { .. } => "pick the conversation back up where you left it",
        }
    }

    /// Which CLI this row would run: the picked agent for everything that
    /// starts something, and the recorded one for a resume.
    ///
    /// The one place that difference is decided, so the row the picker *draws*
    /// and the process the exec *becomes* cannot disagree — which is exactly
    /// the failure this would otherwise have: a title reading Codex over a row
    /// that runs Claude.
    pub fn agent(self, picked: Agent) -> Agent {
        match self {
            Candidate::Launch { .. } | Candidate::Create(_) => picked,
            Candidate::Resume { agent } => agent,
        }
    }

    /// How the row's invocation reads in the picker's skill column: the skill
    /// a launch or creation routes to, or the way back into a conversation.
    ///
    /// Replaces a `route()` that could only answer for the two arms that run a
    /// skill. A resume runs none — that is the whole difference between it and
    /// [`Mode::Plain`], which starts a fresh bare session in the same place —
    /// so the column shows the argv it actually execs.
    pub fn invocation(self, picked: Agent) -> String {
        let agent = self.agent(picked);
        match self {
            Candidate::Launch { route, .. } => route.invocation(agent),
            Candidate::Create(kind) => kind.route().invocation(agent),
            Candidate::Resume { .. } => {
                format!("{} {}", agent.program(), agent.resume_argv().join(" "))
            }
        }
    }

    /// What the text field means while this row is picked: launch rows steer
    /// the agent; creation rows take the task or the seed; a resume takes the
    /// first thing the rejoined session hears, which both CLIs accept beside
    /// their resume flag.
    pub fn field(self) -> &'static str {
        match self {
            Candidate::Launch { .. } | Candidate::Resume { .. } => "steer",
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
    /// Which CLI executes the route. The route and the agent are independent:
    /// choosing Codex changes how the same bundled workflow is invoked, not
    /// what work the cursor selected.
    agent: Agent,
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
    pub fn picked(agent: Agent, mode: Mode, steer: &str) -> LaunchMode {
        let steer = steer.trim();
        LaunchMode {
            agent,
            mode,
            steer: (!steer.is_empty()).then(|| steer.to_string()),
        }
    }

    /// Which CLI will execute this launch.
    pub fn agent(&self) -> Agent {
        self.agent
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
///
/// This is also the stage the launch *hands the agent* in its context block,
/// and deliberately the same type rather than a copy of it: a handed context
/// claiming a done node is a compile error rather than a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Launchable {
    Ready,
    Building,
    InReview,
    NeedsAttention,
}

impl Launchable {
    /// Every launchable stage, compiler-complete
    /// (`every_variant` (src/model.rs)): the iteration the launch matrix and
    /// the doc vocabulary run over, so a stage cannot exist unlaunched (#133).
    pub fn every() -> Vec<Launchable> {
        crate::model::every_variant!(Launchable: Ready, Building, InReview, NeedsAttention)
    }

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
///
/// It is also what a launch hands its agent as the `aim` of the context block
/// (#124), serialized **directly**. A parallel `CtxAim` mirroring these arms
/// would be the same sum written twice and free to drift; one type serialized
/// once cannot disagree with itself, and the wire spelling is pinned by the
/// golden literals in this module's tests rather than by a second declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Aim {
    /// The cluster header: the map itself, charted or driven as a whole.
    Map,
    /// One ticket in the map, carrying the pair its route turns on — and the
    /// facts an agent would otherwise rediscover from the tracker before it
    /// could start: what the ticket is called, and which PRs are linked to it.
    ///
    /// What is *not* here is the claim. There is no assignee and no ticket
    /// status, so the one fact whose staleness is dangerous — is this still
    /// mine to take — cannot be read out of a handed context at all, and
    /// "orient from it, verify live" is a shape rather than a rule to
    /// remember. Blockers are absent for a different reason: the picker
    /// refuses a blocked node before anything is staged, so a launched ticket
    /// never has open ones.
    Ticket {
        number: u64,
        title: String,
        ticket_type: TicketType,
        stage: Launchable,
        prs: Vec<PrLink>,
    },
}

impl Aim {
    /// One value of every arm, compiler-complete
    /// (`every_variant` (src/model.rs)) — the wire only ever sees the tag,
    /// so the ticket representative's payload is the doc guards' business to
    /// vary, not this list's.
    pub fn every_arm() -> Vec<Aim> {
        crate::model::every_variant!(Aim:
            Map,
            Ticket => Aim::Ticket {
                number: 1,
                title: String::new(),
                ticket_type: TicketType::Build,
                stage: Launchable::Ready,
                prs: vec![],
            },
        )
    }
}

/// The map a launch was picked in: its identity *and* its title (#124).
///
/// Carries the whole [`MapId`] rather than a bare issue number because a
/// ticket can sit on a map in another repo — the tracker models it and the
/// fetch parses it — so a number alone would point a cross-repo launch at
/// whatever issue happens to hold that number in the ticket's own repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MapRef {
    /// Flattened, so the map reads as one object (`repo`, `number`, `title`)
    /// rather than an identity nested inside a wrapper.
    #[serde(flatten)]
    pub id: MapId,
    /// The map issue's title as the picker showed it.
    pub title: String,
}

impl MapRef {
    /// The map under `id`, titled as the cluster header reads.
    pub fn new(id: &MapId, title: &str) -> MapRef {
        MapRef {
            id: id.clone(),
            title: title.to_string(),
        }
    }
}

/// What a launch hands its agent in the prompt's `ctx:` block (#124): a
/// snapshot of what `wf` already knew at exec time, so the session's first
/// tracker call can be the **claim** rather than a rediscovery of the map, the
/// ticket and its PRs.
///
/// A borrowed, serialize-only view over the launch's own facts — never a
/// parallel copy of them. Nothing here is a new fact: `wf` fetched every field
/// to draw the row the human picked.
///
/// Deliberately **not** carrying a snapshot instant. No consumer in the
/// contract reads one — the staleness guard is the mandatory live claim, not a
/// timestamp comparison — and reading a wall clock would make prompt building
/// non-deterministic for no gain.
#[derive(Debug, Serialize)]
struct LaunchContext<'a> {
    /// Schema version, and the reading agent's first gate: a `v` it does not
    /// recognise means discard the block whole and discover as it always did.
    v: u32,
    /// The ticket's own repo, full slug — the anchor a reader compares
    /// against its pinned `$REPO` before trusting anything else here.
    repo: &'a str,
    map: &'a MapRef,
    aim: &'a Aim,
}

/// The schema this binary writes. Bumped only when a reader that understands
/// the old shape would misread the new one.
const CONTEXT_VERSION: u32 = 1;

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
        // `Mid` collapses the table for `Auto`'s reason: the launched session
        // manages whatever the node needs — deciding, asking, and driving a
        // build's stages through `wf-tdd` and `wf-review` itself — so the
        // stage's own skill would do one stage and stop. Routing a build node
        // to `Tdd` here would also draw two picker rows naming `/wf-tdd` with
        // different blurbs, which is exactly what a per-row route prevents.
        (Aim::Map | Aim::Ticket { .. }, Mode::Mid) => Route::WayfinderMid,
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
}

/// What the picker was opened on (#114). A project is not a node with a
/// missing issue number — it has no aim, no map and nothing to launch, because
/// nothing has been filed in it yet — so the two are arms of one sum rather
/// than a node struct with optional fields. Which rows the picker offers falls
/// out of this: a node launches (and a map also creates), a bare repo only
/// creates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedAt {
    /// A node the tracker knows about.
    Node {
        /// What the launch picker names: the map, or one ticket in it.
        aim: Aim,
        /// The map of the cluster the row was picked in (#50) — which map a
        /// ticket listed twice was launched from, and the launch target itself
        /// when the cursor was on the cluster header.
        map: MapRef,
        /// The conversation a previous launch of this node left on this
        /// machine, if there is one (#35).
        ///
        /// **Here rather than on [`Staged`]**, which is what makes a resume
        /// row on a project stop unrepresentable: a project names no node, so
        /// there is nothing whose conversation this could be. The picker's
        /// resume row is built from this field alone, so a node nobody has
        /// launched cannot be offered one.
        resume: Option<Resume>,
    },
    /// A whole project: the repo-level stop every project screen opens on,
    /// and the only stop creation hangs off.
    Project,
}

impl Staged {
    /// Stage a launch of `ticket`, picked in the cluster of `map`. `None` for a
    /// finished ticket, which has no launchable stage — the one refusal, made
    /// here so that everything downstream is total.
    pub fn ticket(ticket: &Ticket, map: &MapRef, stage: Stage) -> Option<Staged> {
        Some(Staged {
            repo: ticket.repo.clone(),
            at: StagedAt::Node {
                aim: Aim::Ticket {
                    number: ticket.number,
                    title: ticket.title.clone(),
                    ticket_type: ticket.ticket_type,
                    stage: Launchable::parse(stage)?,
                    prs: ticket.prs.clone(),
                },
                map: map.clone(),
                resume: None,
            },
        })
    }

    /// Stage a launch of a whole map — the cursor was on its cluster header.
    /// Total, unlike the ticket case: a map has no stage to be finished at,
    /// and a finished map is not on screen to put the cursor on.
    pub fn map(map: &MapRef) -> Staged {
        Staged {
            repo: map.id.repo.clone(),
            at: StagedAt::Node {
                aim: Aim::Map,
                map: map.clone(),
                resume: None,
            },
        }
    }

    /// Attach the conversation a previous launch of this node left (#35) — the
    /// one thing that puts a resume row in the picker.
    ///
    /// A builder rather than a constructor argument because the two node
    /// constructors above are total without it: a node that has never run is
    /// the ordinary case, and `None` is not a decision anyone needs to make at
    /// every call site. On a project stop this is a **no-op** — there is no
    /// node to have left a conversation, and the field only exists in the node
    /// arm, so nothing can be attached rather than something being quietly
    /// dropped.
    #[must_use]
    pub fn with_resume(mut self, resume: Resume) -> Staged {
        if let StagedAt::Node { resume: slot, .. } = &mut self.at {
            *slot = Some(resume);
        }
        self
    }

    /// The conversation this stop can be resumed into, if any.
    pub fn resume(&self) -> Option<&Resume> {
        match &self.at {
            StagedAt::Node { resume, .. } => resume.as_ref(),
            StagedAt::Project => None,
        }
    }

    /// Stage a project — the repo-level stop. There is no node, so the picker
    /// offers creation alone.
    pub fn project(repo: &str) -> Staged {
        Staged {
            repo: repo.to_string(),
            at: StagedAt::Project,
        }
    }

    /// How the staged stop reads to the human: the ticket's title, the map's,
    /// or nothing at all for a project, which the repo already names.
    ///
    /// Derived rather than stored (#124). The title the picker draws and the
    /// title the launch hands the agent are one fact, and a second copy beside
    /// the aim would be a field free to disagree with it.
    pub fn title(&self) -> &str {
        match &self.at {
            StagedAt::Node {
                aim: Aim::Ticket { title, .. },
                ..
            } => title,
            StagedAt::Node {
                aim: Aim::Map, map, ..
            } => &map.title,
            // The repo already leads the picker's title, and a project stop
            // adds nothing to it: what is being named is the repo itself. It
            // read `no map` while this stop existed only as the map-less door;
            // it is now every project's row, and most of them have maps.
            StagedAt::Project => "",
        }
    }

    /// Which skill launching this node in `mode` would run — `None` for the
    /// project stop, which has no node to launch and therefore no route.
    pub fn route(&self, mode: Mode) -> Option<Route> {
        match &self.at {
            StagedAt::Node { aim, .. } => Some(route(aim, mode)),
            StagedAt::Project => None,
        }
    }

    /// The row the picker opens on: the first row this stop offers.
    ///
    /// On a node that is its **resume** row when a previous launch left a
    /// conversation there (#35), and the default mode's launch row otherwise —
    /// creation is never the default, because `enter` on a node still means
    /// "work this node" first. On the project row there is no node to launch,
    /// so the first creation row leads — and since that row is where an
    /// untouched cursor sits, it is what `enter` type `enter` runs.
    ///
    /// # Panics
    ///
    /// Never: [`Staged::candidates`] is never empty. Every stop offers rows —
    /// its launch rows, or, on a project row, its creation rows.
    pub fn default_candidate(&self) -> Candidate {
        *self.candidates().first().expect("every stop offers rows")
    }

    /// The picker's rows for this stop, in on-screen order — the one
    /// constructor of [`Candidate`], which is what makes an inconsistent row
    /// unbuildable (#114).
    ///
    /// A **node launches, a project creates**, and neither does the other's
    /// job. Creation used to ride along on the cluster header's picker, on the
    /// grounds that a header was the only repo-level stop there was; a project
    /// row is a *better* one, and having both would put "new map" on every
    /// header of a repo that has three maps open — three doors to one act,
    /// none of them the repo. So the header's picker is the mode rows again,
    /// exactly like a ticket's, and the only difference between them is what
    /// they aim at.
    ///
    /// A node you have launched before **leads** with its resume (#35), which
    /// is the one place this list is not simply the modes. `enter enter` on a
    /// ticket you were working an hour ago should put you back in that
    /// conversation rather than open a second one beside it — the same
    /// argument that put the project row under the untouched cursor, applied
    /// to the picker: the default is the likeliest act, and starting over is
    /// one arrow away.
    pub fn candidates(&self) -> Vec<Candidate> {
        match &self.at {
            StagedAt::Node { aim, resume, .. } => resume
                .iter()
                .map(|r| Candidate::Resume { agent: r.agent })
                .chain(Mode::all().into_iter().map(|mode| Candidate::Launch {
                    mode,
                    route: route(aim, mode),
                }))
                .collect(),
            // A project names nothing that exists yet, so there is nothing to
            // launch — only the ways to start something.
            StagedAt::Project => CreationKind::all()
                .into_iter()
                .map(Candidate::Create)
                .collect(),
        }
    }

    /// How the staged stop reads: `#<n>` for a node — the ticket's number or
    /// the map's — and `+new` for a project, which has no number to
    /// name until a skill files one.
    pub fn key(&self) -> String {
        match &self.at {
            StagedAt::Node {
                aim: Aim::Map, map, ..
            } => format!("#{}", map.id.number),
            StagedAt::Node {
                aim: Aim::Ticket { number, .. },
                ..
            } => format!("#{number}"),
            StagedAt::Project => "+new".to_string(),
        }
    }

    /// The `dl` workspace **launching this node** would run in, known at stage
    /// time — what the prewarm warms, and `None` when there is no single
    /// answer to warm.
    ///
    /// Known this early because a node's workspace depends only on the node:
    /// not on which checkout the second enter picks, and not on the mode. The
    /// two `None` cases are the ones where staging does not determine a
    /// workspace:
    ///
    /// - **A project row** ([`StagedAt::Project`]) offers creation rows
    ///   alone. There is no node, so there is nothing a launch would attach
    ///   to.
    /// - A **creation** picked on a map's picker resolves to the repo's bare
    ///   default workspace rather than the node's (see the resolved launch's
    ///   own `workspace`), so a map stop has two possible answers and staging
    ///   cannot tell which the human will take. It is still warmed on the
    ///   node's, because that is what the default row launches and what
    ///   `enter enter` therefore does; arrowing down to a creation row
    ///   instead leaves the warmed container unused, which is the same cost
    ///   as backing out.
    pub fn node_workspace(&self) -> Option<String> {
        match &self.at {
            StagedAt::Node { aim, map, .. } => Some(node_workspace_name(
                &self.repo,
                match aim {
                    Aim::Map => map.id.number,
                    Aim::Ticket { number, .. } => *number,
                },
            )),
            StagedAt::Project => None,
        }
    }
}

/// Where the selected agent runs: on the host, as `wf` always has, or — for a
/// compatible Claude launch — inside the checkout's own devcontainer by way of
/// `dl` (#80).
///
/// Two states, not three: there is no "wanted isolation but could not get it".
/// [`Isolation::detect`] is total — it answers with what will actually happen,
/// so a launch cannot carry an intention the exec then fails to honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// No compatible isolation path: the selected agent runs in the checkout
    /// directly.
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
    /// All three conditions are required, and an unusable `dl` **degrades to
    /// the host** rather than refusing the launch: a repo may carry a
    /// `devcontainer.json` for its editor users on a machine that has never
    /// heard of `dl`, and isolation here is for dependencies, not security
    /// (#73), so the host is a worse environment rather than an unsafe one. The
    /// launch notice names the mode ([`Launch::describe`]), so the degradation
    /// is visible.
    ///
    /// The third condition — usable, not merely on PATH — is there because
    /// "installed" and "speaks this binary's command line" are different
    /// questions, and answering only the first one moved the failure past the
    /// point where it could still degrade. The floor `dl` is held to, and the
    /// release that made one necessary, are recorded on `DEVLAUNCH_FLOOR`.
    ///
    /// Codex is deliberately host-only for now. `dl` mounts the host's
    /// `~/.claude` into a workspace — which carries Claude's authentication and
    /// the relative skill copies — but does not mount `~/.codex`. Running Codex
    /// through it would show a real picker choice and then fail to find either
    /// its login or `$wf`; the host is the only path this binary can honestly
    /// make work until `dl` grows a Codex handover.
    pub fn detect(checkout: &Path, agent: Agent) -> Isolation {
        if agent == Agent::Claude
            && has_devcontainer(checkout)
            && devlaunch_on_path() == Devlaunch::Usable
        {
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

/// The oldest `dl` whose command line this binary actually speaks.
///
/// Raised whenever `wf` starts calling something an older `dl` does not have,
/// and that is not hypothetical: [`prewarm`] fires `dl <workspace> up`, and
/// `up` arrived in devlaunch **0.0.24**. Before this floor existed, a machine
/// with everything up to date ran `wf` against the released `dl` 0.0.23,
/// satisfied every condition [`Isolation::detect`] tested, and then failed
/// inside the prewarm on an argument that release had never heard of — the one
/// failure mode a PATH lookup cannot see, because the name was there.
///
/// A floor is the honest expression of a subprocess dependency: `wf` cannot
/// pin `dl`'s version the way a linked crate would (devlaunch#53), so it
/// checks what it found and degrades when the answer is too old.
/// Public because it is half of a contract with another repository, and
/// `tests/live_devlaunch.rs` holds the two halves against each other: the pixi
/// `floor` environment pins `devlaunch` to exactly this number, and the test
/// fails if the pin and this constant ever name different releases. A floor
/// nothing is ever run at is a floor nobody has checked.
pub const DEVLAUNCH_FLOOR: DlVersion = DlVersion(0, 0, 24);

/// What `wf` execs to carry an agent into a container: `dl <ws> -- <command>`.
///
/// The workspace spec is a plain argv entry and needs no quoting; the agent
/// command after `--` does, because `dl` runs it through a shell, and it is one
/// entry rather than several because "a shell command" is exactly what `dl`
/// documents it to be.
///
/// Named rather than built inline so `tests/live_devlaunch.rs` can hand *this*
/// to a real `dl`. An argv a contract test spells out for itself only proves
/// the test agrees with the test.
///
/// The `workspace` that test passes is its own — a devpod id rather than the
/// `owner/repo@branch` spec every real caller here builds, because the spec
/// form makes `dl` clone. What comes from here is the shape after it.
pub fn isolated_argv(workspace: &str, agent: &[String]) -> Vec<String> {
    vec![
        DEVLAUNCH.to_string(),
        workspace.to_string(),
        "--".to_string(),
        agent
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
    ]
}

/// What [`prewarm`] spawns to build the container ahead of the launch.
///
/// `up` is the verb that made [`DEVLAUNCH_FLOOR`] necessary: it arrived in
/// devlaunch 0.0.24, and a `wf` that sent it to 0.0.23 got
/// `Unknown command 'up'` from inside a detached process nobody was watching.
/// Named here for the same reason as [`isolated_argv`], and the contract test
/// sends it to a real 0.0.23 to watch exactly that happen.
pub fn prewarm_argv(workspace: &str) -> Vec<String> {
    vec![
        DEVLAUNCH.to_string(),
        workspace.to_string(),
        "up".to_string(),
    ]
}

/// A `dl` version, ordered by the three numbers `dl --version` prints.
///
/// A tuple struct rather than three fields because the derived [`Ord`] is
/// exactly the comparison wanted — major, then minor, then patch — and there
/// is nothing else to say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DlVersion(u32, u32, u32);

impl std::fmt::Display for DlVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl DlVersion {
    /// The version in `dl --version`'s first line, or `None` when there is not
    /// one to compare.
    ///
    /// Tolerant on purpose, because two installs answer differently and both
    /// are legitimate: the released build prints `dl 0.0.24`, while an editable
    /// dev install prints `dl 0.0.24 (dev, editable from /path/to/checkout)` —
    /// devlaunch makes that trailer the way the two are told apart, so a parser
    /// that insisted on a bare version would refuse isolation to exactly the
    /// person developing `dl`. A pre-release tail on the patch number
    /// (`0.0.24rc1`) reads as 0.0.24, because that is the version carrying the
    /// feature.
    fn parse(stdout: &str) -> Option<DlVersion> {
        let mut parts = stdout
            .lines()
            .next()?
            .split_whitespace()
            .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))?
            .split('.');
        let number = |part: Option<&str>| -> Option<u32> {
            part?
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        };
        Some(DlVersion(
            number(parts.next())?,
            number(parts.next())?,
            number(parts.next())?,
        ))
    }
}

/// What the `dl` on this machine is, as far as a launch is concerned.
///
/// Four states, because "there but unusable" is a real answer and the only one
/// worth explaining to anybody. [`Isolation`] stays two-state about the
/// *outcome* — what will actually happen — and this is the type that carries
/// why, so neither has to pretend the other's job is simple.
///
/// Public for `tests/live_devlaunch.rs`, which is the only place this
/// classification is ever applied to a `dl` that actually exists. Every other
/// test of it — and every shimmed probe — hands it a string this repo wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Devlaunch {
    /// Not on PATH, or on PATH and not runnable. The ordinary state of a
    /// machine that never installed it, and there is no version to report.
    Absent,
    /// On PATH, but `--version` answered with nothing this binary can compare:
    /// a future `dl` that changed the format, or some other program of that
    /// name.
    Unreadable,
    /// On PATH, and older than [`DEVLAUNCH_FLOOR`].
    TooOld(DlVersion),
    /// On PATH and new enough for what `wf` asks of it.
    Usable,
}

impl Devlaunch {
    /// Read `dl --version`'s answer.
    ///
    /// Split from the probe so the rule is testable without a `dl` on the
    /// machine running the tests — the same split `enabled_from` below makes for
    /// `WF_PREWARM`.
    pub fn from_version_output(stdout: &str) -> Devlaunch {
        match DlVersion::parse(stdout) {
            None => Devlaunch::Unreadable,
            Some(found) if found < DEVLAUNCH_FLOOR => Devlaunch::TooOld(found),
            Some(_) => Devlaunch::Usable,
        }
    }

    /// What to add to the launch notice — the half of a degradation that an
    /// absent `(devlaunch)` suffix cannot carry.
    ///
    /// `None` for the two states nobody can act on from here: [`Usable`] has
    /// nothing to explain, and [`Absent`] is #80's "a repo may carry a
    /// `devcontainer.json` for its editor users", where a line on every launch
    /// would be noise about a tool the user never asked for. What is left is a
    /// `dl` that *is* installed and cannot be used, which is both fixable and
    /// otherwise indistinguishable from `wf` quietly ignoring the devcontainer.
    ///
    /// [`Usable`]: Devlaunch::Usable
    /// [`Absent`]: Devlaunch::Absent
    pub fn shortfall(self) -> Option<String> {
        match self {
            Devlaunch::Absent | Devlaunch::Usable => None,
            Devlaunch::TooOld(found) => Some(format!(
                " — dl {found} is older than the {DEVLAUNCH_FLOOR} this wf needs, so it ran on the host"
            )),
            Devlaunch::Unreadable => Some(format!(
                " — `{DEVLAUNCH} --version` did not answer with a version, so it ran on the host"
            )),
        }
    }

    /// Does this `dl` say what every clone it made would lose?
    ///
    /// [`reap`](crate::reap) asks, because a missing answer means opposite
    /// things either side of [`UNSAVED_IS_AN_OBJECT`]: before it, `null` on
    /// `dl`'s own clone was the ordinary *clean* case; from it, `null` appears
    /// exactly where `devlaunch` is false, so a `null` on a clone `dl` made
    /// means `dl`'s own inspection fell over. Reaping is right in the first
    /// reading and destroys work in the second, and no single row can tell them
    /// apart — only the version can, which is why this is asked here rather
    /// than guessed in the parser.
    ///
    /// The two states with no version to compare answer `false`, so anything
    /// this binary cannot place reads the old, permissive way. The failure mode
    /// is then "behaves as it did before the floor existed" rather than
    /// "refuses every workspace on a machine `wf` could not probe".
    pub fn answers_unsaved(self) -> bool {
        match self {
            Devlaunch::Usable => true,
            // Too old for the *floor* is not too old for this question: a `dl`
            // that answers `unsaved` but lacks some later subcommand still
            // answered. Compared rather than assumed, so raising the floor
            // cannot quietly restate a fact about a past release.
            Devlaunch::TooOld(found) => found >= UNSAVED_IS_AN_OBJECT,
            Devlaunch::Absent | Devlaunch::Unreadable => false,
        }
    }
}

/// Ask `dl` its version, once per process.
///
/// Memoized because [`Isolation::detect`] is called per candidate checkout —
/// both the checkout picker and [`prewarm`] walk every tree of a repo — and
/// each probe starts a Python interpreter (~90ms, measured in devlaunch#53).
/// Nothing is lost by answering once: `PATH` is fixed when `wf` is exec'd, and
/// a `dl` upgraded mid-session would not change the launch already staged.
///
/// `output()` rather than `status()` because the version is the answer, and
/// because it keeps `dl`'s stdio off a screen the TUI owns.
fn devlaunch_on_path() -> Devlaunch {
    static PROBED: OnceLock<Devlaunch> = OnceLock::new();
    *PROBED.get_or_init(|| {
        let Ok(program) = resolve_on_path(DEVLAUNCH) else {
            return Devlaunch::Absent;
        };
        match unstamped(program).arg("--version").output() {
            Ok(answer) => Devlaunch::from_version_output(&String::from_utf8_lossy(&answer.stdout)),
            // On PATH but it would not run — a file that is not executable, or
            // not a program. There is no version to report, so this is the same
            // state as never having been installed.
            Err(_) => Devlaunch::Absent,
        }
    })
}

/// The release where `--ls --json` began answering `unsaved` for every clone
/// `dl` made.
///
/// devlaunch 0.0.24 replaced a string-or-null field with a one-key object, and
/// from there `unsaved` is null *exactly* where `devlaunch` is false — there is
/// no clone of `dl`'s own to inspect. That is the same release [`prewarm`]'s
/// `up` arrived in, so this equals [`DEVLAUNCH_FLOOR`] today.
///
/// Two constants for one release anyway, because they answer two questions.
/// Raising the floor is about what `wf` may *ask* `dl` to do; this is about how
/// to read what `dl` already said, and [`reap`](crate::reap) reads listings
/// from whatever `dl` is on PATH rather than only from one a launch would
/// accept. Collapsing them would make a future floor bump silently restate a
/// fact about a past release.
pub const UNSAVED_IS_AN_OBJECT: DlVersion = DlVersion(0, 0, 24);

/// Does the `dl` on this machine say what every clone it made would lose?
///
/// Split from the probe for the same reason [`Devlaunch::from_version_output`]
/// is — the rule is the part worth testing, and it is testable without a `dl`
/// on the machine running the tests.
pub(crate) async fn devlaunch_answers_unsaved() -> bool {
    off_runtime(|| devlaunch_on_path().answers_unsaved()).await
}

/// Run one blocking call without stalling the async runtime it is awaited on.
///
/// The version probe starts a Python interpreter and waits ~90ms for it
/// (devlaunch#53), and [`reap::workspaces`](crate::reap::workspaces) reaches it
/// from a tokio worker inside the picker's background survey — memoized, so it
/// is one stall per process, but one stall of a worker thread is still every
/// timer and channel on that worker arriving late.
async fn off_runtime<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .expect("the blocking call does not panic")
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

/// The instant the keystroke that resolved to an exec landed, and the instant
/// `wf` fired a prewarm for that node, if it fired one (#160).
///
/// Two stamps of things **`wf` itself did**, and nothing else. Neither is a
/// claim about how the launch went: whether the prewarm was still running, was
/// already finished, or saved this launch nothing at all is visible only to the
/// process that then had to run the launch, and `dl` decides it from the arm it
/// takes. A `wf` that reported a "prewarm hit" would be reporting on a
/// container it fired and forgot and never saw again.
///
/// They travel in the **environment of the exec'd process**, not in the `ctx:`
/// block: `ctx:` is addressed to the agent, and these are addressed to the
/// timing reader inside `dl`, which reads them at the top of its own `main`.
/// The variable names and the spelling of the values are `dl`'s to mint
/// (blooop/devlaunch#194); `wf` is the writer, and a writer that invented its
/// own names would be handing the reader nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff {
    /// The last keystroke before the exec — the one whose meaning was "run it",
    /// after which nothing waits on a human. `dl` reports the gap from here to
    /// its own start as the one stage that measures the exec itself.
    t0: SystemTime,
    /// When staging fired `dl <workspace> up` for the node being launched, if
    /// staging fired one. Absent is the ordinary case and says exactly that:
    /// nothing was warmed, so there is nothing for `dl` to weigh its launch
    /// against.
    prewarm_fired: Option<SystemTime>,
}

impl Handoff {
    /// Stamp the keystroke that has just resolved to an exec.
    ///
    /// The only public constructor, so a handoff always carries the clock at
    /// the moment it was taken — never one assembled after the fact, which
    /// would silently fold whatever came between into the gap `dl` measures.
    /// The prewarm's instant is passed in because it happened earlier, and
    /// only the session that fired it knows when.
    pub fn now(prewarm_fired: Option<SystemTime>) -> Handoff {
        Handoff {
            t0: SystemTime::now(),
            prewarm_fired,
        }
    }

    /// The variables this seam owns, in the order the docs publish them.
    ///
    /// One list, read by both the exec path and the docs guard, so the names a
    /// launch sets and the names the README teaches cannot drift apart.
    pub fn variables() -> [&'static str; 2] {
        [HANDOFF_T0_VAR, PREWARM_FIRED_VAR]
    }
}

/// The keystroke stamp's variable: see [`Handoff`].
const HANDOFF_T0_VAR: &str = "DEVLAUNCH_HANDOFF_T0";

/// The prewarm stamp's variable: see [`Handoff`].
const PREWARM_FIRED_VAR: &str = "DEVLAUNCH_PREWARM_FIRED_AT";

/// A child `wf` starts and does **not** become, with the seam's stamps
/// (#160) stripped out of its environment.
///
/// Every `dl` `wf` runs except the launch itself is one of these — the
/// `--version` probe, the prewarm's `dl <ws> up`, and `wf reap`'s listing and
/// removals — and none of them is a hand-over, so none may arrive carrying a
/// keystroke stamp. Declining to *set* one is not enough, because a child
/// inherits `wf`'s own environment and `wf` is routinely run from inside a
/// workspace whose environment `dl` stamped for the launch that created it: an
/// agent doing that would have every `dl` it ran report a hand-over from a
/// keystroke hours old, indistinguishable on the far side from a real one.
/// This is the same reason [`Launch::stamps`] hands `exec` a *removal* rather
/// than a skip.
///
/// One constructor rather than a removal remembered at each site, so a `dl`
/// child added later cannot quietly reopen it — and it takes the program name
/// rather than a whole argv because one caller runs `dl` and another runs the
/// `sh` that backgrounds it ([`spawn_detached`]), which is the same seam and a
/// different program.
pub fn unstamped(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for var in Handoff::variables() {
        command.env_remove(var);
    }
    command
}

/// One stamp as the seam spells it: seconds since the Unix epoch, to nine
/// decimal places — the string `date +%s.%N` prints, which is the format the
/// reader on the other side parses.
///
/// `None` for an instant before the epoch, which is a clock this side has no
/// business describing: the far side would read it as a handoff that began
/// decades ago, and a fiction in a trend is worse than a stage nobody reported.
fn epoch_seconds(at: SystemTime) -> Option<String> {
    let since = at.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(format!("{}.{:09}", since.as_secs(), since.subsec_nanos()))
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
        /// The map the node was picked in — `/wf`'s first argument, its only
        /// one when the aim is the map itself, and half of what the launch
        /// hands the agent (#124).
        map: MapRef,
        /// The skill this launch execs, resolved from (type, stage).
        route: Route,
        /// What the launch picker settled on. The mode half already picked
        /// `route`; what is left to spend here is the steering text.
        mode: LaunchMode,
    },
    /// Start something new in the repo — the skill files the issues.
    Create { creation: Creation, agent: Agent },
    /// Rejoin the conversation a previous launch of this node left (#35).
    ///
    /// Carries the node's number so [`Launch::workspace`] resolves to the same
    /// per-node workspace the original launch ran in — which is the whole
    /// mechanism, since that workspace is the cwd both CLIs key their
    /// conversation history by. No aim, no map and no route: a resume names
    /// nothing to the agent, because the session it is rejoining already knows
    /// all of it.
    Resume {
        number: u64,
        agent: Agent,
        /// The first thing the rejoined session hears, or nothing at all.
        prompt: Option<String>,
    },
}

impl Job {
    /// The issue number this job hangs off: the ticket's, or the map's when the
    /// whole map is what was picked. `None` for a creation, which has no number
    /// until the launched skill files one.
    ///
    /// Held here rather than inlined at each use because the display key and
    /// the `dl` branch name must never disagree about which number a launch is
    /// — they are two renderings of the same fact.
    fn number(&self) -> Option<u64> {
        match self {
            Job::Node { aim, map, .. } => Some(match aim {
                Aim::Map => map.id.number,
                Aim::Ticket { number, .. } => *number,
            }),
            Job::Create { .. } => None,
            Job::Resume { number, .. } => Some(*number),
        }
    }

    /// The CLI that executes this job, held on every arm because neither
    /// creation nor resume has a launch mode of its own.
    fn agent(&self) -> Agent {
        match self {
            Job::Node { mode, .. } => mode.agent(),
            Job::Create { agent, .. } | Job::Resume { agent, .. } => *agent,
        }
    }
}
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
        match self.job.number() {
            Some(number) => format!("{}#{}", short_repo(&self.repo), number),
            None => format!("{}+new", short_repo(&self.repo)),
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
    ///
    /// Public because it is also the key a session's prewarm record is kept
    /// under ([`crate::app::App::prewarm_fired`]): asking "was *this* launch's
    /// container warmed" is asking about this name, and computing it a second
    /// way at the asking site is how the two would come to disagree.
    pub fn workspace(&self) -> String {
        match self.job.number() {
            Some(number) => node_workspace_name(&self.repo, number),
            None => self.repo.clone(),
        }
    }

    /// Where this launch's agent runs.
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }

    /// Which agent will run this resolved launch.
    pub fn agent(&self) -> Agent {
        self.job.agent()
    }

    /// One-line description for the notice: what is being launched, where, and
    /// — when it is not the host default — in what. "Where" is where the agent
    /// actually works: the checkout on the host, the per-node workspace in a
    /// container.
    ///
    /// A host launch of a tree that *declared* a devcontainer also says why it
    /// is not isolated, when there is a reason worth stating (`dl` installed
    /// but too old to use). Here rather than at the three call sites in
    /// `app.rs`, so no notice can be built that leaves it out.
    pub fn describe(&self) -> String {
        let place = match self.isolation {
            Isolation::Host => self.cwd.display().to_string(),
            Isolation::Devlaunch => self.workspace(),
        };
        let what = match &self.job {
            Job::Node { .. } => self.key(),
            // A creation has no `#n` to name yet, so the notice names the act.
            Job::Create { creation, .. } => creation.kind().label().to_string(),
            // A resume has a number, so it names the node *and* says that it
            // is going back rather than starting: the two are one keystroke
            // apart in the picker and the notice is the last chance to see
            // which one was taken.
            Job::Resume { .. } => format!("resume {}", self.key()),
        };
        // A host launch of a tree with no devcontainer wanted no container, so
        // there is nothing to explain; only the trees that asked get a reason.
        let shortfall = match self.isolation {
            Isolation::Host if has_devcontainer(&self.cwd) => devlaunch_on_path().shortfall(),
            Isolation::Host | Isolation::Devlaunch => None,
        };
        format!(
            "{what} in {place}{}{}",
            self.isolation.suffix(),
            shortfall.unwrap_or_default()
        )
    }

    /// The selected agent and its optional one-argument prompt. Both CLIs
    /// receive a whole skill invocation as one argv entry; a plain launch with
    /// no steering receives no prompt at all.
    fn agent_argv_inner(&self) -> Vec<String> {
        let agent = self.agent();
        // A resume is the one shape whose argv is not
        // `<agent> <bypass> [prompt]`: Codex spells it as a subcommand, and a
        // subcommand has to come before the switches it takes.
        let prompt = match &self.job {
            Job::Node { mode, .. } => mode.opening_prompt(self.skill_invocation()),
            Job::Create { creation, .. } => Some(creation.invocation(agent)),
            Job::Resume { prompt, .. } => {
                let mut argv = vec![agent.program().to_string()];
                argv.extend(agent.resume_argv().iter().map(|a| (*a).to_string()));
                argv.push(agent.skip_permissions().to_string());
                argv.extend(prompt.clone());
                return argv;
            }
        };
        let mut argv = vec![
            agent.program().to_string(),
            agent.skip_permissions().to_string(),
        ];
        argv.extend(prompt);
        argv
    }

    /// The record this launch leaves for a later resume (#35) — `None` for a
    /// creation, which has no node to key one on until its skill files one.
    ///
    /// Taken from the resolved launch rather than from the picker's intent, so
    /// what is remembered is the tree the agent actually ran in — including
    /// which of several checkouts the human picked, which is precisely the bit
    /// that decides whose conversation comes back.
    pub fn session(&self) -> Option<Session> {
        Some(Session::new(
            self.repo.clone(),
            self.job.number()?,
            self.agent(),
            self.cwd.clone(),
            self.isolation,
        ))
    }

    /// The selected agent's skill invocation, its arguments, and the context
    /// block that follows them (#124). Plain mode has no skill, while creation
    /// prompts are built by [`Creation`].
    ///
    /// The block goes **after** the skill's own arguments and before any
    /// ` steer: …` suffix [`LaunchMode::opening_prompt`] adds, which is the
    /// whole grammar: `steer:`'s existing "everything after this is the
    /// human's text" rule is undisturbed, and a steer containing the letters
    /// `ctx:` cannot be mistaken for a block.
    ///
    /// A creation is handed none because it names nothing that exists yet, and
    /// a plain session because there is no skill for it to be addressed to.
    ///
    /// The `expect` on the serializer is unreachable: [`LaunchContext`] is a
    /// fixed set of strings, integers and unit enums, so every failure
    /// `serde_json` defines for a serializer — a non-string map key, a
    /// non-finite float, a `Serialize` impl that errors — is impossible here.
    fn skill_invocation(&self) -> Option<String> {
        let Job::Node {
            aim, map, route, ..
        } = &self.job
        else {
            return None;
        };
        route.bundled_skill()?;
        let skill = route.invocation(self.agent());
        let invocation = match (route, aim) {
            // `/wf-one` is a creation's route, and its own doc says a `wf-one`
            // line never carries a block — it names work that does not exist
            // on the tracker yet. A node routed here is unreachable from the
            // picker but representable ([`plan`] takes any [`Route`]), so the
            // answer is given here rather than left to depend on that.
            (Route::One, _) => return Some(skill),
            (_, Aim::Map) => format!("{skill} {}", map.id.number),
            (Route::Tdd | Route::Review, Aim::Ticket { number, .. }) => format!("{skill} {number}"),
            (
                Route::Wayfinder | Route::WayfinderMid | Route::WayfinderAuto,
                Aim::Ticket { number, .. },
            ) => {
                format!("{skill} {} {number}", map.id.number)
            }
            (Route::Plain, _) => return None,
        };
        let ctx = serde_json::to_string(&LaunchContext {
            v: CONTEXT_VERSION,
            repo: &self.repo,
            map,
            aim,
        })
        .expect("the launch context is plain data and always serializes");
        Some(format!("{invocation} ctx: {ctx}"))
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
        let agent = self.agent_argv_inner();
        match self.isolation {
            Isolation::Host => agent,
            Isolation::Devlaunch => isolated_argv(&self.workspace(), &agent),
        }
    }

    /// What this launch tells the process it is about to become about the
    /// keystroke it came from ([`Handoff`]) — one entry per seam variable, in
    /// the published order, and `None` for one this launch has nothing to say
    /// with.
    ///
    /// **A stamp is set only where the exec *is* the `dl` that reads it.** A
    /// host launch execs the agent itself: nothing there parses these, and one
    /// left sitting in an agent session's environment would be picked up by
    /// every unrelated `dl` that session goes on to run — each reporting a
    /// handoff measured from a keystroke hours old. So the host arm says
    /// nothing, and `None` is applied as a *removal* rather than a skip
    /// ([`Launch::exec`]), which is what keeps a stamp `wf` inherited from
    /// travelling on under `wf`'s name.
    pub fn stamps(&self, handoff: &Handoff) -> [(&'static str, Option<String>); 2] {
        let carried = |at: Option<SystemTime>| match self.isolation {
            Isolation::Devlaunch => at.and_then(epoch_seconds),
            Isolation::Host => None,
        };
        let [t0, prewarm_fired] = Handoff::variables();
        [
            (t0, carried(Some(handoff.t0))),
            (prewarm_fired, carried(handoff.prewarm_fired)),
        ]
    }

    /// Become the selected agent: replace `wf`'s process image with it — or,
    /// for an isolated Claude launch, with the `dl` that carries it into the
    /// container — in the checkout.
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
    /// The [`Handoff`] is an argument for the same reason: its `t0` is the
    /// keystroke, not this moment, and everything between the two — the
    /// shutdowns, the cache write, the skill refresh — is part of what the far
    /// side measures. A stamp taken here would quietly leave all of it out.
    ///
    /// # Panics
    ///
    /// Never in practice: [`agent_argv`](Self::agent_argv) builds the vector
    /// literally and always starts it with the program name, so the split below
    /// cannot come up empty. The `expect` is there to say so.
    pub fn exec(&self, handoff: &Handoff) -> anyhow::Error {
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

        let mut command = Command::new(&program);
        command.args(rest).current_dir(&self.cwd);
        // The seam (#160). Written onto the child rather than into `wf`'s own
        // environment, so nothing here can be read back by anything but the
        // process this is about to become — and removed, not skipped, where
        // this launch has no stamp of its own to hand over.
        for (var, stamp) in self.stamps(handoff) {
            match stamp {
                Some(value) => command.env(var, value),
                None => command.env_remove(var),
            };
        }
        // `CommandExt::exec` only ever returns on failure.
        let err = command.exec();
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

/// Find `program` on `$PATH`, skipping empty entries and any candidate the OS
/// would not execute.
///
/// The execute bit matters because this stands in for execvp, which keeps
/// searching past a file it cannot run — so a stray non-executable `claude`
/// early on PATH must not fail a launch the OS would have made.
///
/// A name containing a separator is a path already and is taken as given —
/// that is the caller naming a file, not `$PATH` resolution.
///
/// Two callers, and the difference matters: [`Launch::exec`] resolves the
/// program it is about to become and reports the miss, while the `dl` probe
/// wants only a path to run `--version` on and treats a miss as "no `dl` here",
/// which [`Isolation::detect`] answers with [`Isolation::Host`].
fn resolve_on_path(program: &str) -> Result<PathBuf, anyhow::Error> {
    resolve_in(program, &std::env::var_os("PATH").unwrap_or_default())
}

/// [`resolve_on_path`] against a given `PATH` value — the part with rules worth
/// testing, split from the one line that reads the environment so the tests
/// never have to mutate it under a parallel suite.
fn resolve_in(program: &str, path: &std::ffi::OsStr) -> Result<PathBuf, anyhow::Error> {
    if program.contains('/') {
        return Ok(PathBuf::from(program));
    }
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| executable(candidate))
        .ok_or_else(|| anyhow::anyhow!("`{program}` is not on PATH — is it installed?"))
}

/// Would execvp accept this candidate? A regular file with any execute bit
/// set. Mode bits rather than `access(2)`'s exact answer, which is the same
/// approximation `which` makes — close enough for a search whose miss is
/// reported, and whose false positive still fails loudly at the `exec` itself.
fn executable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    candidate
        .metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// A launch prompt with the JSON of every `ctx:` block replaced by `…` (#124).
///
/// Test-only, and a reading aid rather than a parser: the block's own bytes
/// are pinned byte-for-byte by the golden literals in this module's tests, so
/// every *other* test — the ones about which skill a node routes to, where the
/// steering suffix lands, and that the prompt is one argv entry — stays about
/// what it is about instead of restating a snapshot. Brace counting is enough
/// because the block is `serde_json`'s own output and no fixture title carries
/// a brace.
#[cfg(test)]
pub(crate) fn eliding_ctx(prompt: &str) -> String {
    let mut out = String::new();
    let mut rest = prompt;
    while let Some((head, json)) = rest.split_once(" ctx: {") {
        out.push_str(head);
        out.push_str(" ctx: …");
        let mut depth = 1usize;
        rest = "";
        for (i, c) in json.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                rest = &json[i + 1..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The name half of a repo slug (`blooop/wayfinder` → `wayfinder`). Display
/// only — never an identity key, because a fork and its upstream share it.
fn short_repo(slug: &str) -> &str {
    slug.split('/').next_back().unwrap_or(slug)
}

/// The per-node `dl` workspace, `owner/repo@wayfinder/<repo>-<n>` — one
/// definition, because two spellings of it would be two workspaces: the
/// prewarm builds a container that [`Launch::workspace`] then has to find by
/// the same name. A creation has no node and does not come through here; it
/// gets the repo's bare default workspace.
fn node_workspace_name(repo: &str, number: u64) -> String {
    format!("{}@wayfinder/{}-{}", repo, short_repo(repo), number)
}

/// The environment variable that turns the prewarm on. See
/// [`prewarm_enabled`] for why it is off by default.
const PREWARM_VAR: &str = "WF_PREWARM";

/// The spellings that turn the prewarm on. An **allowlist**, which is where
/// this deliberately parts company with `dl`'s `DEVLAUNCH_NO_TOOLS` and its
/// "anything that is not `0`/`false`/`no`" rule.
///
/// The two variables point opposite ways, and the safe reading of a value
/// nobody anticipated goes with the direction. `DEVLAUNCH_NO_TOOLS` is an
/// opt-*out*, so treating an unrecognised value as "set" disables a
/// convenience — cheap, and cheap in the safe direction. `WF_PREWARM` is an
/// opt-*in* to something that creates containers and clones, so the same rule
/// would make `WF_PREWARM=off` start building containers. Unrecognised means
/// off here, and only these say yes.
const TRUTHY: [&str; 4] = ["1", "true", "yes", "on"];

/// Whether staging a launch may start its container early. **Off unless
/// `WF_PREWARM` is set to something that is not `0`/`false`/`no`.**
///
/// Opt-in, and deliberately so. The prewarm turns the *first* enter — a
/// keystroke that until now only opened an overlay — into real local state:
/// a work branch in `dl`'s cache, a full clone of the repo, and a running
/// container. That is an excellent trade when you are working a map and a bad
/// one when you are browsing it, because a launch you back out of leaves all
/// three behind — and `wf reap` will not collect them while the ticket is
/// open. Nobody should discover that by upgrading.
///
/// It does reach the network (a fetch, an image pull); what it never does is
/// *publish*. `dl` creates the work branch locally and never pushes it, so an
/// abandoned stage leaves nothing on GitHub.
pub fn prewarm_enabled() -> bool {
    enabled_from(std::env::var(PREWARM_VAR).ok().as_deref())
}

/// The reading half of [`prewarm_enabled`], split out so the rule can be
/// tested without mutating the process environment under parallel tests.
fn enabled_from(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(value) => TRUTHY.contains(&value.trim().to_ascii_lowercase().as_str()),
    }
}

/// The `dl` invocation that makes a staged node's container ready before the
/// second enter: `dl <workspace> up` — start or create, never attach.
///
/// `None` unless there is exactly one thing to warm. Two ways to get nothing:
/// the stop names no launchable workspace ([`Staged::node_workspace`] — the
/// project row), or no candidate checkout would launch isolated, which is a
/// host launch with no container in it.
///
/// Any *one* isolated candidate is enough — the workspace name is the node's,
/// whichever checkout gets picked. If the human then picks a host tree, the
/// container is left standing, which is the same fate as a launch they cancel
/// outright: [`crate::reap`] only collects workspaces whose tickets are
/// closed, so an abandoned stage of an open ticket waits for a hand to remove
/// it.
///
/// This plans the bet; [`prewarm_enabled`] decides whether it is placed. When
/// it is, `devpod up` — image pull, container create, tool install, the
/// seconds-to-minutes tail of every cold launch — runs while the human is
/// still choosing a mode and typing steer text. `dl` serializes the launch
/// that follows against it (a per-workspace lock), so the second enter
/// attaches to the container the prewarm built instead of racing it.
pub fn prewarm(checkouts: &[Checkout], staged: &Staged) -> Option<Vec<String>> {
    let workspace = staged.node_workspace()?;
    let isolated = candidate_checkouts(checkouts, &staged.repo)
        .into_iter()
        .any(|c| Isolation::detect(&c.path, Agent::default()) == Isolation::Devlaunch);
    isolated.then(|| prewarm_argv(&workspace))
}

/// Run `argv` in the background, detached from this process and its terminal.
///
/// Three things have to be true, and a plain `Command::spawn` gives none of
/// them:
///
/// - **It cannot touch the terminal.** The TUI owns the screen, so the child's
///   stdio is the null device. That alone is not enough: a child can open
///   `/dev/tty` directly, which `git` and `ssh` do when they want a passphrase,
///   and `dl` shells out to both. Putting it in its own process group makes the
///   kernel stop that read with `SIGTTIN` instead of letting it race `wf`'s own
///   `event::read()` for keystrokes.
/// - **`wf`'s signals are not its signals.** `ctrl-c` and a closed terminal go
///   to the *foreground process group*. A prewarm in that group dies mid-clone,
///   and `dl`'s cleanup of a half-written workspace runs in a Python `except`
///   that a signal does not reach — so the next launch finds a workspace
///   directory that exists and is not usable. Its own group is what keeps a
///   quit from corrupting the cache.
/// - **It must leave nothing behind.** `main` is explicit that nothing may
///   outlive the `exec` as a zombie the agent then holds, and `exec` does not
///   reparent children. So this double-forks: the direct child is a shell that
///   backgrounds the real command and exits immediately, which reparents the
///   command to init and lets the shell be waited for right here. The wait is
///   a process spawn and exit — milliseconds — and it is what makes this
///   leave no entry in anyone's process table.
///
/// Failure is silent by design: it costs the head start and nothing else,
/// because the launch that follows runs the same `dl` path itself and reports
/// whatever is actually wrong.
pub fn spawn_detached(argv: &[String]) {
    let Some((program, _rest)) = argv.split_first() else {
        return;
    };
    // Resolved here for the same reason `exec` does it: an empty `$PATH` entry
    // would otherwise let a cloned repo's own `./dl` be what gets run.
    let Ok(resolved) = resolve_on_path(program) else {
        return;
    };
    let mut command = std::iter::once(resolved.display().to_string())
        .chain(argv[1..].iter().cloned())
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ");
    command.push_str(" >/dev/null 2>&1 &");

    let spawned = unstamped("sh")
        .arg("-c")
        .arg(&command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    // The shell exits as soon as it has backgrounded the command, so this
    // returns immediately — and reaps it, leaving no zombie for the agent.
    if let Ok(mut shell) = spawned {
        let _ = shell.wait();
    }
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
/// never prompts. The aim, mode **and route** all arrive already settled — the
/// route comes from the [`Candidate`] the human actually saw drawn, not from a
/// second derivation here, so what execs is what the row named (#114). This
/// function answers only *where* the agent can run.
pub fn plan(checkouts: &[Checkout], staged: &Staged, route: Route, mode: &LaunchMode) -> Targets {
    let StagedAt::Node { aim, map, .. } = &staged.at else {
        // Unreachable from the picker: [`Staged::candidates`] offers no launch
        // row on a project stop, so nothing there can ask for a node
        // launch. Refusing rather than inventing a node — there is no aim and
        // no map issue to invent one from — keeps this total.
        return Targets::Unregistered;
    };
    resolve(
        checkouts,
        &staged.repo,
        &Job::Node {
            aim: aim.clone(),
            map: map.clone(),
            route,
            mode: mode.clone(),
        },
    )
}

/// Resolve a creation against the projects cache — the same rules as [`plan`]:
/// zero or one candidate checkout never prompts. The creation arrives already
/// complete ([`CreationKind::with_text`] refused the empty task), so this
/// function only answers *where* the skill runs.
pub fn plan_create(
    checkouts: &[Checkout],
    repo: &str,
    creation: &Creation,
    agent: Agent,
) -> Targets {
    resolve(
        checkouts,
        repo,
        &Job::Create {
            creation: creation.clone(),
            agent,
        },
    )
}

/// Resolve a resume of the staged node (#35) — `None` when the stop has no
/// conversation to go back to, which is every stop the picker never drew a
/// resume row on.
///
/// **Deliberately not a [`Targets`]**, and the only launch path that is not.
/// Everything else asks the cache *where* the agent could run and prompts when
/// several trees answer; a resume is not a choice about where, because the
/// conversation exists in exactly one place and the record says which. So this
/// never consults [`candidate_checkouts`], never prompts, and cannot resolve
/// into a tree the original launch did not use — including when the human has
/// since registered a second checkout of the same repo, which is the case a
/// checkout picker would silently get wrong half the time.
///
/// Isolation is re-detected rather than stored: it is a fact about the tree,
/// the tree is the same one, and a fresh reading cannot disagree with itself
/// the way a stored copy could once a devcontainer was added or `dl` removed.
pub fn resume_launch(staged: &Staged, steer: &str) -> Option<Launch> {
    let resume = staged.resume()?;
    let number = match &staged.at {
        StagedAt::Node {
            aim: Aim::Map, map, ..
        } => map.id.number,
        StagedAt::Node {
            aim: Aim::Ticket { number, .. },
            ..
        } => *number,
        // Unreachable: `Staged::resume` already answered `None` here.
        StagedAt::Project => return None,
    };
    let steer = steer.trim();
    Some(Launch {
        repo: staged.repo.clone(),
        cwd: resume.checkout.clone(),
        job: Job::Resume {
            number,
            agent: resume.agent,
            prompt: (!steer.is_empty()).then(|| steer.to_string()),
        },
        isolation: resume.isolation,
    })
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
fn resolve(checkouts: &[Checkout], repo: &str, job: &Job) -> Targets {
    let launches: Vec<Launch> = candidate_checkouts(checkouts, repo)
        .into_iter()
        .map(|c| Launch {
            repo: repo.to_string(),
            cwd: c.path.clone(),
            // The job is the same whichever checkout hosts it — what differs
            // per candidate is only where it runs and in what.
            job: job.clone(),
            isolation: Isolation::detect(&c.path, job.agent()),
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
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::model::{classify, Checks, PrLink, PrStatus, Review, Status, TicketType};

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

    /// The map every ticket fixture is picked in — its own repo, number and
    /// title, all three of which the launch hands the agent.
    fn map_ref(number: u64) -> MapRef {
        MapRef::new(
            &MapId::new("blooop/wayfinder", number),
            "the dev-process tree",
        )
    }

    fn checkout(path: &str, repo: &str) -> Checkout {
        Checkout::new(PathBuf::from(path), repo.to_string())
    }

    /// What the picker composes with the default mode row selected, steered by
    /// whatever was typed into it.
    fn interactive(steer: &str) -> LaunchMode {
        LaunchMode::picked(Agent::Claude, Mode::Interactive, steer)
    }

    /// The same with the `auto` row selected.
    fn auto(steer: &str) -> LaunchMode {
        LaunchMode::picked(Agent::Claude, Mode::Auto, steer)
    }

    /// The same with the `plain` row selected — the launch that hands the
    /// session no skill at all.
    fn plain(steer: &str) -> LaunchMode {
        LaunchMode::picked(Agent::Claude, Mode::Plain, steer)
    }

    /// An interactive `/wf` plan — the default launch, and the shape
    /// every checkout-resolution test wants (route and mode are orthogonal to
    /// which trees are candidates).
    fn plan_wf(checkouts: &[Checkout], ticket: &Ticket, map_issue: u64) -> Targets {
        let staged =
            Staged::ticket(ticket, &map_ref(map_issue), Stage::Ready).expect("ready is launchable");
        plan_picked(
            checkouts,
            &staged,
            &LaunchMode::picked(Agent::Claude, Mode::Interactive, ""),
        )
    }

    /// [`plan`] as the picker reaches it: the route comes from the row the
    /// stop would have drawn for this mode, not from a second derivation the
    /// test invents — so these stay tests of the real second enter.
    fn plan_picked(checkouts: &[Checkout], staged: &Staged, mode: &LaunchMode) -> Targets {
        let route = staged.route(mode.mode()).expect("a node stop launches");
        plan(checkouts, staged, route, mode)
    }

    fn cache() -> Vec<Checkout> {
        vec![
            checkout("/data/k1/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/k2/kinisi_ros", "kinisi/kinisi_ros"),
            checkout("/data/proj/wayfinder", "blooop/wayfinder"),
            checkout("/data/proj/dotfiles", "upstream/dotfiles"),
        ]
    }

    /// A conversation a previous launch left in the wayfinder checkout.
    fn resume(agent: Agent) -> Resume {
        Resume {
            agent,
            checkout: PathBuf::from("/data/proj/wayfinder"),
            isolation: Isolation::Host,
            at: 1_000,
        }
    }

    /// The staged ticket the resume tests work: node #117 of map #47, with
    /// whatever conversation the cache had for it.
    fn staged_with(resume: Option<Resume>) -> Staged {
        let staged = Staged::ticket(
            &ticket("blooop/wayfinder", 117),
            &map_ref(47),
            Stage::Building,
        )
        .expect("building is launchable");
        match resume {
            Some(resume) => staged.with_resume(resume),
            None => staged,
        }
    }

    #[test]
    fn a_node_you_have_launched_before_leads_its_picker_with_the_way_back() {
        // The whole point of the row: come back to a ticket you were working
        // and `enter enter` rejoins the conversation rather than starting a
        // second one beside it. So resume is not merely present, it *leads* —
        // an untouched picker cursor sits on it.
        let staged = staged_with(Some(resume(Agent::Claude)));
        assert_eq!(
            staged.candidates().first(),
            Some(&Candidate::Resume {
                agent: Agent::Claude
            })
        );
        assert_eq!(
            staged.default_candidate(),
            Candidate::Resume {
                agent: Agent::Claude
            }
        );
        // And every way to start fresh is still there, in order, one arrow
        // away.
        let modes: Vec<Candidate> = staged.candidates().into_iter().skip(1).collect();
        assert_eq!(modes, staged_with(None).candidates());
    }

    #[test]
    fn a_node_nobody_has_launched_offers_no_way_back() {
        // The row is unrepresentable without the record it is built from, so
        // this is the compiler's claim as much as the test's: nothing here
        // can invent a resume for a node that has never run.
        let staged = staged_with(None);
        assert!(staged
            .candidates()
            .iter()
            .all(|c| !matches!(c, Candidate::Resume { .. })));
        assert_eq!(
            staged.default_candidate(),
            Candidate::Launch {
                mode: Mode::Interactive,
                // The fixture is a task, so interactive routes to /wf — the
                // point is only that the default is a *launch* row again.
                route: Route::Wayfinder,
            },
            "with nothing to rejoin, the default is what it always was"
        );
    }

    #[test]
    fn a_project_row_has_no_node_and_therefore_no_way_back() {
        // Creation rows name work that does not exist yet, so there is no
        // conversation for them to rejoin. Attaching one is not an error, it
        // is a no-op: the field lives in the node arm, so the state cannot be
        // built in the first place.
        let staged = Staged::project("blooop/wayfinder").with_resume(resume(Agent::Claude));
        assert_eq!(
            staged.candidates(),
            Staged::project("blooop/wayfinder").candidates()
        );
        assert!(staged
            .candidates()
            .iter()
            .all(|c| !matches!(c, Candidate::Resume { .. })));
    }

    #[test]
    fn resuming_execs_each_agents_own_way_back_and_nothing_else() {
        // The flags are the contract with the two CLIs, so they are pinned
        // literally. Both are cwd-scoped — `claude --continue` continues "the
        // most recent conversation in the current directory", and `codex
        // resume` filters by cwd unless `--all` — which is what makes a
        // per-node workspace sufficient and a session id unnecessary.
        // One rule for both: the program, how it is told to go back, then the
        // switches. Codex forces it — `resume` is a subcommand and has to
        // precede the flags it takes — and Claude follows the same shape
        // rather than keeping the bypass in the second slot every *other*
        // launch puts it in, so there is one ordering here instead of two.
        let claude = resumed(Agent::Claude, "");
        assert_eq!(
            claude.agent_argv(),
            vec!["claude", "--continue", "--dangerously-skip-permissions"]
        );
        let codex = resumed(Agent::Codex, "");
        assert_eq!(
            codex.agent_argv(),
            vec![
                "codex",
                "resume",
                "--last",
                "--dangerously-bypass-approvals-and-sandbox"
            ]
        );
    }

    #[test]
    fn a_resume_carries_the_typed_text_as_the_prompt_it_wakes_up_on() {
        // Both CLIs take a prompt beside the resume flag, so the picker's text
        // field keeps meaning something on this row: rejoin *and* say this.
        // It is the whole prompt, not a ` steer: …` suffix — there is no skill
        // invocation in front of it for a suffix to be addressed to.
        assert_eq!(
            resumed(Agent::Claude, "run the tests").agent_argv().last(),
            Some(&"run the tests".to_string())
        );
        assert_eq!(
            resumed(Agent::Codex, "run the tests").agent_argv().last(),
            Some(&"run the tests".to_string())
        );
    }

    #[test]
    fn a_resume_hands_no_context_block_because_the_conversation_already_has_one() {
        // #124's block exists to save a fresh session its rediscovery. A
        // resumed session did that discovery already, and re-handing it would
        // be telling an agent mid-conversation what it worked out an hour ago.
        let prompt = resumed(Agent::Claude, "carry on").agent_argv().join(" ");
        assert!(!prompt.contains("ctx:"), "{prompt}");
        assert!(
            !prompt.contains("/wf"),
            "a resume invokes no skill: {prompt}"
        );
    }

    #[test]
    fn a_resume_goes_back_into_the_node_workspace_it_was_launched_in() {
        // The conversation is keyed by cwd, so the way back is the same
        // workspace — never the repo's default one, which is a different tree
        // with a different conversation in it.
        let launch = Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Resume {
                number: 117,
                agent: Agent::Claude,
                prompt: None,
            },
            isolation: Isolation::Devlaunch,
        };
        let argv = launch.agent_argv();
        assert_eq!(argv[0], "dl");
        assert_eq!(argv[1], "blooop/wayfinder@wayfinder/wayfinder-117");
        assert_eq!(argv[2], "--");
        assert_eq!(
            argv[3], "'claude' '--continue' '--dangerously-skip-permissions'",
            "the agent command still crosses dl's shell quoted"
        );
    }

    #[test]
    fn a_resume_goes_back_the_way_it_went_out_not_the_way_the_tree_looks_now() {
        // Isolation is **recorded**, never re-detected. An isolated launch's
        // conversation lives in the container, at a cwd inside `dl`'s own
        // clone — so if the host checkout later loses its `.devcontainer/`, a
        // fresh detection would answer Host and quietly resume *the
        // checkout's* conversation instead, which is a different one and
        // possibly the human's own. `/nonexistent` has no devcontainer and
        // could never detect as isolated, which is exactly the point.
        let recorded = Resume {
            agent: Agent::Claude,
            checkout: PathBuf::from("/nonexistent/proj/wayfinder"),
            isolation: Isolation::Devlaunch,
            at: 1_000,
        };
        let launch = resume_launch(&staged_with(Some(recorded)), "").expect("resolves");
        assert_eq!(
            launch.agent_argv().first().map(String::as_str),
            Some("dl"),
            "a resume must go back into the container it ran in"
        );
        // And the other way: a launch that ran on the host stays on the host,
        // even once someone adds a devcontainer to that tree. Its conversation
        // is in the checkout, and a container would be a fresh one.
        let recorded = Resume {
            agent: Agent::Claude,
            checkout: PathBuf::from("/nonexistent/proj/wayfinder"),
            isolation: Isolation::Host,
            at: 1_000,
        };
        let launch = resume_launch(&staged_with(Some(recorded)), "").expect("resolves");
        assert_eq!(
            launch.agent_argv().first().map(String::as_str),
            Some("claude")
        );
    }

    #[test]
    fn a_node_launch_leaves_the_session_a_later_resume_is_offered_on() {
        // The record is written from the launch itself rather than from the
        // picker's intent, so what is remembered is what actually ran.
        let launch = match plan_wf(&cache(), &ticket("blooop/wayfinder", 117), 47) {
            Targets::One(launch) => launch,
            other => panic!("one checkout, got {other:?}"),
        };
        let session = launch.session().expect("a node launch is resumable");
        assert_eq!(session.repo, "blooop/wayfinder");
        assert_eq!(session.number, 117);
        assert_eq!(session.resume.agent, Agent::Claude);
        assert_eq!(
            session.resume.checkout,
            PathBuf::from("/data/proj/wayfinder")
        );
    }

    #[test]
    fn a_creation_leaves_no_session_because_it_names_no_node_yet() {
        // `/wf-one` files its ticket after the exec, so at record time there
        // is no number to key a resume on. A sentinel zero would put a lie in
        // the cache and offer node #0 a way back.
        let creation = Creation::Task {
            task: "add a flag".to_string(),
        };
        let launch = match plan_create(&cache(), "blooop/wayfinder", &creation, Agent::Claude) {
            Targets::One(launch) => launch,
            other => panic!("one checkout, got {other:?}"),
        };
        assert_eq!(launch.session(), None);
    }

    #[test]
    fn resuming_a_node_re_records_it_so_the_way_back_stays_the_latest_one() {
        // A resume is itself a launch, and the conversation it leaves behind
        // is the one to come back to next time.
        assert_eq!(
            resumed(Agent::Codex, "").session().map(|s| s.number),
            Some(117)
        );
    }

    /// A resolved resume of node #117, as the second enter builds it.
    fn resumed(agent: Agent, steer: &str) -> Launch {
        let staged = staged_with(Some(resume(agent)));
        resume_launch(&staged, steer).expect("a staged resume resolves")
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
                        aim: Aim::Ticket { number: 16, .. },
                        ..
                    } if launch.job.number() == Some(16)
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
            elided_argv(&launch),
            vec![
                "claude".to_string(),
                Agent::Claude.skip_permissions().to_string(),
                "/wf 1 16 ctx: …".to_string()
            ]
        );
    }

    #[test]
    fn the_agent_runs_codex_with_a_skill_mention_and_one_prompt_argument() {
        let staged = Staged::ticket(&ticket("blooop/wayfinder", 16), &map_ref(1), Stage::Ready)
            .expect("ready is launchable");
        let launch = match plan(
            &cache(),
            &staged,
            Route::Wayfinder,
            &LaunchMode::picked(Agent::Codex, Mode::Interactive, "try it"),
        ) {
            Targets::One(launch) => launch,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            elided_argv(&launch),
            vec![
                "codex".to_string(),
                Agent::Codex.skip_permissions().to_string(),
                "$wf 1 16 ctx: … steer: try it".to_string(),
            ]
        );
    }

    #[test]
    fn a_picked_launch_keeps_who_decides_out_of_what_was_typed() {
        // The two axes (#62/#96), now that the mode is a picked row and not a
        // word at the front of one line: the selection decides who resolves the
        // node, and the text only ever steers.
        let picked = |mode: LaunchMode| (mode.agent, mode.mode, mode.steer);
        assert_eq!(
            picked(interactive("")),
            (Agent::Claude, Mode::Interactive, None)
        );
        assert_eq!(picked(auto("")), (Agent::Claude, Mode::Auto, None));
        // An all-whitespace field is an empty one, not a steer made of spaces.
        assert_eq!(
            picked(interactive("   ")),
            (Agent::Claude, Mode::Interactive, None)
        );
        assert_eq!(picked(auto("  \t ")), (Agent::Claude, Mode::Auto, None));
        assert_eq!(
            picked(auto("skip the flaky suite")),
            (
                Agent::Claude,
                Mode::Auto,
                Some("skip the flaky suite".to_string())
            )
        );
        assert_eq!(
            picked(interactive("try the other approach")),
            (
                Agent::Claude,
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
                (Agent::Claude, Mode::Interactive, Some(typed.to_string())),
                "{typed:?} steers an interactive launch"
            );
        }
    }

    #[test]
    fn a_ticket_picker_lists_exactly_the_launch_modes() {
        // #114: creation is a repo-level act, and a ticket is not a repo-level
        // stop — its picker stays the pure mode list, concerns unmerged. The
        // rows are `Mode::all` in its own order, so they read as rising
        // autonomy rather than as the order the modes were added in.
        let staged = Staged::ticket(&ticket("blooop/wayfinder", 16), &map_ref(1), Stage::Ready)
            .expect("launchable");
        assert_eq!(
            staged.candidates(),
            vec![
                Candidate::Launch {
                    mode: Mode::Interactive,
                    route: Route::Wayfinder
                },
                Candidate::Launch {
                    mode: Mode::Mid,
                    route: Route::WayfinderMid
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
    fn a_node_launches_and_a_project_creates_and_neither_does_the_others_job() {
        // The two kinds of stop, and the whole of what tells their pickers
        // apart. A cluster header is a *map* — a node — so it walks the three
        // modes exactly as a ticket does; creation belongs to the project row,
        // which has no node and therefore no launch row. Each candidate is
        // complete, carrying its own resolved route (the `Targets::Many`
        // move), so a row and its launch cannot disagree.
        let header = Staged::map(&map_ref(59)).candidates();
        assert_eq!(
            header,
            vec![
                Candidate::Launch {
                    mode: Mode::Interactive,
                    route: Route::Wayfinder
                },
                Candidate::Launch {
                    mode: Mode::Mid,
                    route: Route::WayfinderMid
                },
                Candidate::Launch {
                    mode: Mode::Auto,
                    route: Route::WayfinderAuto
                },
                Candidate::Launch {
                    mode: Mode::Plain,
                    route: Route::Plain
                },
            ],
            "a map's picker carries no creation rows"
        );

        let project = Staged::project("blooop/wayfinder").candidates();
        assert_eq!(
            project,
            vec![
                Candidate::Create(CreationKind::Task),
                Candidate::Create(CreationKind::Map),
                Candidate::Create(CreationKind::MapMid),
                Candidate::Create(CreationKind::MapAuto),
            ]
        );
        // Every row names the skill it execs — the creation rows' routes are
        // their own rather than any node's.
        let shown: Vec<String> = project
            .iter()
            .map(|c| c.invocation(Agent::Claude))
            .collect();
        assert_eq!(shown, ["/wf-one", "/wf", "/wf-mid", "/wf-auto"]);
    }

    #[test]
    fn a_build_tickets_launch_rows_resolve_its_own_stage_routes() {
        // Complete candidates mean the per-row route is the (aim, mode) answer
        // for *this* node: a build ticket's interactive row reads /wf-tdd, not
        // a generic default.
        let mut node = ticket("blooop/wayfinder", 16);
        node.ticket_type = TicketType::Build;
        let staged = Staged::ticket(&node, &map_ref(1), Stage::Ready).expect("launchable");
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
            Candidate::Create(CreationKind::MapMid).label(),
            "new map, mid"
        );
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
        assert_eq!(Candidate::Create(CreationKind::MapMid).field(), "seed");
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
            CreationKind::MapMid.with_text(""),
            Some(Creation::MapMid { seed: None })
        );
        assert_eq!(
            CreationKind::MapAuto.with_text(""),
            Some(Creation::MapAuto { seed: None })
        );
    }

    /// The one-checkout creation launch, reduced to its argv.
    fn creation_argv(kind: CreationKind, text: &str) -> Vec<String> {
        let creation = kind.with_text(text).expect("a buildable creation");
        match plan_create(&cache(), "blooop/wayfinder", &creation, Agent::Claude) {
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
                Agent::Claude.skip_permissions().to_string(),
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
            creation_argv(CreationKind::MapMid, "")
                .last()
                .expect("prompt"),
            "/wf-mid"
        );
        assert_eq!(
            creation_argv(CreationKind::MapMid, "a caching layer")
                .last()
                .expect("prompt"),
            "/wf-mid a caching layer"
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
        match plan_create(&cache(), "blooop/wayfinder", &creation, Agent::Claude) {
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
            plan_create(&cache(), "blooop/dotfiles", &creation(), Agent::Claude),
            Targets::Unregistered
        );
        match plan_create(&cache(), "kinisi/kinisi_ros", &creation(), Agent::Claude) {
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
            vec![Mode::Interactive, Mode::Mid, Mode::Auto, Mode::Plain]
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
        let staged = Staged::ticket(&node, &map_ref(1), stage).expect("a launchable stage");
        match plan_picked(&cache(), &staged, mode) {
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

    /// A prompt with its context block elided — see [`eliding_ctx`].
    fn elided(prompt: &str) -> String {
        eliding_ctx(prompt)
    }

    /// The whole argv of a launch, each entry read the same way.
    fn elided_argv(launch: &Launch) -> Vec<String> {
        launch.agent_argv().iter().map(|a| elided(a)).collect()
    }

    /// The whole argv of a launch aimed at the whole map.
    fn map_argv(mode: &LaunchMode) -> Vec<String> {
        let staged = Staged::map(&map_ref(59));
        match plan_picked(&cache(), &staged, mode) {
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
            elided(&ticket_prompt(
                TicketType::Build,
                Stage::Ready,
                &interactive("")
            )),
            "/wf-tdd 16 ctx: …"
        );
        assert_eq!(
            elided(&ticket_prompt(
                TicketType::Build,
                Stage::InReview,
                &interactive("")
            )),
            "/wf-review 16 ctx: …"
        );
        assert_eq!(
            elided(&ticket_prompt(
                TicketType::Grilling,
                Stage::Ready,
                &interactive("")
            )),
            "/wf 1 16 ctx: …"
        );
        assert_eq!(
            elided(&ticket_prompt(
                TicketType::Grilling,
                Stage::Ready,
                &auto("")
            )),
            "/wf-auto 1 16 ctx: …"
        );
        // Steering rides as a suffix, whatever the route.
        assert_eq!(
            elided(&ticket_prompt(
                TicketType::Grilling,
                Stage::Ready,
                &auto("skip the flaky suite")
            )),
            "/wf-auto 1 16 ctx: … steer: skip the flaky suite"
        );
        assert_eq!(
            elided(&ticket_prompt(
                TicketType::Build,
                Stage::Ready,
                &interactive("try the other approach")
            )),
            "/wf-tdd 16 ctx: … steer: try the other approach"
        );
    }

    /// The context block a build launch of the fixture node hands its agent —
    /// the whole `ctx:` argument, spelt out rather than rebuilt from the
    /// serializer, so a renamed field or a re-spelt variant fails here (#124).
    const BUILD_CTX: &str = concat!(
        r#"{"v":1,"repo":"blooop/wayfinder","#,
        r#""map":{"repo":"blooop/wayfinder","number":1,"title":"the dev-process tree"},"#,
        r#""aim":{"ticket":{"number":16,"title":"the ticket","#,
        r#""ticket_type":"build","stage":"ready","prs":[]}}}"#
    );

    /// The `ctx:` block of a prompt, or `None` when it carries none.
    fn ctx_of(prompt: &str) -> Option<&str> {
        let block = prompt.split_once(" ctx: ")?.1;
        Some(match block.split_once(" steer: ") {
            Some((ctx, _)) => ctx,
            None => block,
        })
    }

    #[test]
    fn a_ticket_launch_hands_the_agent_what_wf_already_knows() {
        // #124: the discovery prelude every ticket launch runs today — which
        // map, which PR, what type and stage — is answered in the prompt
        // itself, so the agent's first tracker call can be the claim. Asserted
        // as the whole literal argument, because *where* the block sits is the
        // grammar: after the skill's own arguments, before any steering text.
        assert_eq!(
            ticket_prompt(TicketType::Build, Stage::Ready, &interactive("")),
            format!("/wf-tdd 16 ctx: {BUILD_CTX}")
        );
        // A decision route keeps its map argument, and the block follows it.
        assert_eq!(
            ticket_prompt(TicketType::Grilling, Stage::Ready, &interactive("")),
            format!(
                "/wf 1 16 ctx: {}",
                BUILD_CTX.replace(r#""ticket_type":"build""#, r#""ticket_type":"grilling""#)
            )
        );
    }

    #[test]
    fn the_context_sits_between_the_skill_arguments_and_the_steering_text() {
        // The `steer:` rule is untouched (#122): everything after it is the
        // human's text, so the block goes in front of it and a steer that
        // itself contains "ctx:" cannot be mistaken for one.
        assert_eq!(
            ticket_prompt(
                TicketType::Build,
                Stage::Ready,
                &interactive("try the other approach")
            ),
            format!("/wf-tdd 16 ctx: {BUILD_CTX} steer: try the other approach")
        );
    }

    #[test]
    fn a_map_launch_is_handed_the_map_and_names_no_ticket() {
        // A map is not a ticket with missing fields: the aim serializes as the
        // bare word, and the map's own identity is carried once, in `map`.
        assert_eq!(
            map_prompt(&interactive("")),
            concat!(
                r#"/wf 59 ctx: {"v":1,"repo":"blooop/wayfinder","#,
                r#""map":{"repo":"blooop/wayfinder","number":59,"title":"the dev-process tree"},"#,
                r#""aim":"map"}"#
            )
        );
    }

    #[test]
    fn the_handed_context_carries_the_prs_a_reviewer_would_otherwise_hunt_for() {
        // The rediscovery this exists to kill: a review launch's argv is a
        // bare ticket number, so finding the PR to diff is a tracker round
        // trip of its own. The link arrives with the launch instead —
        // cross-repo capable, because the tracker's links are.
        let mut node = ticket("blooop/wayfinder", 16);
        node.ticket_type = TicketType::Build;
        node.prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status: PrStatus::Open {
                checks: Checks::Passing,
                review: Review::Approved,
            },
        }];
        let staged =
            Staged::ticket(&node, &map_ref(1), Stage::InReview).expect("in review launches");
        let prompt = match plan_picked(&cache(), &staged, &interactive("")) {
            Targets::One(l) => l.agent_argv().last().expect("a prompt").clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            prompt,
            concat!(
                r#"/wf-review 16 ctx: {"v":1,"repo":"blooop/wayfinder","#,
                r#""map":{"repo":"blooop/wayfinder","number":1,"title":"the dev-process tree"},"#,
                r#""aim":{"ticket":{"number":16,"title":"the ticket","ticket_type":"build","#,
                r#""stage":"in_review","prs":[{"repo":"blooop/wayfinder","number":90,"#,
                r#""status":{"open":{"checks":"passing","review":"approved"}}}]}}}"#
            )
        );
    }

    /// Every field name a serialized block contains, at any depth — including
    /// the tag keys the enums write (`ticket`, `open`).
    fn keys_of(value: &serde_json::Value) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut stack = vec![value];
        while let Some(node) = stack.pop() {
            match node {
                serde_json::Value::Object(fields) => {
                    for (key, child) in fields {
                        keys.insert(key.clone());
                        stack.push(child);
                    }
                }
                serde_json::Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
        keys
    }

    #[test]
    fn the_handed_context_cannot_speak_about_the_claim() {
        // The one fact whose staleness is dangerous — is this ticket still
        // mine to take — is absent from the *type*, not merely from this
        // fixture, so "trust it for orientation, re-read before mutating" is a
        // shape rather than a rule to remember.
        //
        // Asserted over the block's *field names*, and as the whole set rather
        // than a list of forbidden words. Both halves matter. A substring scan
        // of the block's text cannot tell a key from a value, so it read
        // `"stage":"needs_attention"` — a legal, spec-mandated value — as the
        // word "needs" and would have failed on a node that is simply awaiting
        // attention. And a blacklist only catches the names someone thought of:
        // an exact key set fails on *any* new field, which is the only way this
        // stays true as the schema grows.
        let with_pr = |stage| {
            let mut node = ticket("blooop/wayfinder", 16);
            node.ticket_type = TicketType::Build;
            node.prs = vec![PrLink {
                repo: "blooop/wayfinder".to_string(),
                number: 90,
                status: PrStatus::Open {
                    checks: Checks::Failing,
                    review: Review::ChangesRequested,
                },
            }];
            let staged = Staged::ticket(&node, &map_ref(1), stage).expect("a launchable stage");
            let prompt = match plan_picked(&cache(), &staged, &interactive("")) {
                Targets::One(l) => l.agent_argv().last().expect("a prompt").clone(),
                other => panic!("{other:?}"),
            };
            let ctx = ctx_of(&prompt)
                .expect("a ticket launch carries context")
                .to_string();
            keys_of(&serde_json::from_str(&ctx).expect("the block is JSON"))
        };
        let named = |names: &[&str]| {
            names
                .iter()
                .map(|n| (*n).to_string())
                .collect::<BTreeSet<String>>()
        };
        // The richest block the schema can produce: a ticket aim with a linked
        // PR open enough to carry both live signals.
        let expected = named(&[
            "v",
            "repo",
            "map",
            "number",
            "title",
            "aim",
            "ticket",
            "ticket_type",
            "stage",
            "prs",
            "status",
            "open",
            "checks",
            "review",
        ]);
        assert_eq!(with_pr(Stage::InReview), expected);
        // The node the old substring guard would have failed on: awaiting
        // attention is a stage, not a claim.
        assert_eq!(with_pr(Stage::NeedsAttention), expected);
        for forbidden in [
            "assignee",
            "assignees",
            "claim",
            "frontier",
            "blocked_by",
            "needs",
        ] {
            assert!(
                !expected.contains(forbidden),
                "{forbidden:?} must be unrepresentable in the handed context"
            );
        }
    }

    /// The golden wire words, one exhaustive `match` per enumerated field.
    ///
    /// These four are the whole vocabulary the tracker doc publishes, written
    /// as literals traceable to it rather than derived from the types — a
    /// derivation would only prove serde agrees with itself. Being `match`es
    /// with no wildcard is half of the guarantee: adding a type, a stage, a
    /// check rollup or a review decision stops this module compiling until
    /// the new word has been decided and pinned here. The other half is the
    /// matrix below iterating [`TicketType::every`] and friends rather than
    /// hand-written arrays, so the new variant is also *launched* — a probe
    /// variant once compiled and greened with its word pinned while the
    /// arrays never exercised it (#133).
    fn type_word(ticket_type: TicketType) -> &'static str {
        match ticket_type {
            TicketType::Build => "build",
            TicketType::Research => "research",
            TicketType::Task => "task",
            TicketType::Grilling => "grilling",
            TicketType::Prototype => "prototype",
            TicketType::Untyped => "untyped",
        }
    }

    /// See [`type_word`].
    fn stage_word(stage: Launchable) -> &'static str {
        match stage {
            Launchable::Ready => "ready",
            Launchable::Building => "building",
            Launchable::InReview => "in_review",
            Launchable::NeedsAttention => "needs_attention",
        }
    }

    /// See [`type_word`].
    fn checks_word(checks: Checks) -> &'static str {
        match checks {
            Checks::Absent => "absent",
            Checks::Pending => "pending",
            Checks::Passing => "passing",
            Checks::Failing => "failing",
        }
    }

    /// See [`type_word`].
    fn review_word(review: Review) -> &'static str {
        match review {
            Review::NotRequired => "not_required",
            Review::Required => "required",
            Review::Approved => "approved",
            Review::ChangesRequested => "changes_requested",
        }
    }

    /// See [`type_word`]. An open PR is the one state that carries more, so
    /// its word is the tag and the two live signals under it.
    fn status_words(status: &PrStatus) -> String {
        match status {
            PrStatus::Draft => r#""status":"draft""#.to_string(),
            PrStatus::Merged => r#""status":"merged""#.to_string(),
            PrStatus::Closed => r#""status":"closed""#.to_string(),
            PrStatus::Open { checks, review } => format!(
                r#""status":{{"open":{{"checks":"{}","review":"{}"}}}}"#,
                checks_word(*checks),
                review_word(*review)
            ),
        }
    }

    /// Every value the block's enumerated fields can hold, launched for real
    /// and spelled out (#122 §4).
    ///
    /// One test over the whole matrix rather than a property run: the
    /// vocabularies are small and closed, so every cell fits, and a golden
    /// literal per cell says what a generator never can — *which* word the wire
    /// uses. Before this, only `ready`, `build` and a single open PR were ever
    /// emitted by any test, so every other word the tracker doc publishes
    /// rested on the doc's say-so.
    ///
    /// "Whole" is the type's own claim, not this test's: the iteration comes
    /// from [`TicketType::every`] and friends, where the compiler holds the
    /// list complete, so a new variant cannot green without a row here (#133).
    #[test]
    fn every_stage_type_and_pr_state_spells_itself_on_the_wire() {
        for ticket_type in TicketType::every() {
            for launchable in Launchable::every() {
                let prompt = ticket_prompt(ticket_type, staged_at(launchable), &interactive(""));
                let ctx = ctx_of(&prompt)
                    .expect("a ticket launch carries context")
                    .to_string();
                let expected = format!(
                    r#""ticket_type":"{}","stage":"{}""#,
                    type_word(ticket_type),
                    stage_word(launchable)
                );
                assert!(ctx.contains(&expected), "expected {expected} in {ctx}");
            }
        }
        let mut pr_states = Vec::new();
        for arm in PrStatus::every_arm() {
            match arm {
                PrStatus::Open { .. } => {
                    for checks in Checks::every() {
                        for review in Review::every() {
                            pr_states.push(PrStatus::Open { checks, review });
                        }
                    }
                }
                settled => pr_states.push(settled),
            }
        }
        for status in pr_states {
            let expected = status_words(&status);
            let ctx = ctx_of(&pr_prompt(status))
                .expect("a ticket launch carries context")
                .to_string();
            assert!(ctx.contains(&expected), "expected {expected} in {ctx}");
        }
        // The other arm of the matrix's aim axis: a map launch names none of
        // the ticket vocabulary at all, rather than nulling it out.
        let map = ctx_of(&map_prompt(&interactive("")))
            .expect("a map launch carries context")
            .to_string();
        assert!(map.ends_with(r#""aim":"map"}"#), "{map}");
        for absent in ["ticket_type", "stage", "prs"] {
            assert!(!map.contains(absent), "a map aim names no {absent}: {map}");
        }
    }

    /// The model stage a launch at this launchable stage is staged from — the
    /// inverse of [`Launchable::parse`], its own wildcard-free `match` so the
    /// matrix can iterate [`Launchable::every`] and still hand
    /// [`Staged::ticket`] the [`Stage`] it wants. A launchable stage no
    /// [`Stage`] maps to cannot be launched at all, and this is where that
    /// refuses to compile.
    fn staged_at(launchable: Launchable) -> Stage {
        match launchable {
            Launchable::Ready => Stage::Ready,
            Launchable::Building => Stage::Building,
            Launchable::InReview => Stage::InReview,
            Launchable::NeedsAttention => Stage::NeedsAttention,
        }
    }

    /// A review-stage launch whose one linked PR stands where the caller says.
    fn pr_prompt(status: PrStatus) -> String {
        let mut node = ticket("blooop/wayfinder", 16);
        node.ticket_type = TicketType::Build;
        node.prs = vec![PrLink {
            repo: "blooop/wayfinder".to_string(),
            number: 90,
            status,
        }];
        let staged = Staged::ticket(&node, &map_ref(1), Stage::InReview).expect("in review");
        match plan_picked(&cache(), &staged, &interactive("")) {
            Targets::One(l) => l.agent_argv().last().expect("a prompt").clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_that_has_no_skill_to_address_is_handed_context() {
        // The block is addressed to a skill. A plain session has none, and a
        // creation names nothing that exists yet — so neither carries one, and
        // the fallback path (a hand-invoked skill, which never saw the picker)
        // stays exactly what it is today.
        assert_eq!(
            ctx_of(
                &ticket_argv(TicketType::Build, Stage::Ready, &plain("rebase onto main")).join(" ")
            ),
            None
        );
        assert_eq!(ctx_of(&map_argv(&plain("")).join(" ")), None);
        for (kind, text) in [
            (CreationKind::Task, "wire the exporter"),
            (CreationKind::Map, "a caching layer"),
            (CreationKind::MapMid, "a caching layer"),
            (CreationKind::MapAuto, ""),
        ] {
            assert_eq!(ctx_of(&creation_argv(kind, text).join(" ")), None);
        }
        // `/wf-one` is the creation route, and the picker never routes a node
        // there — but `plan` takes any route, so the block's absence is decided
        // by the route rather than by that being hard to reach. `wf-one`'s own
        // doc forbids the block, and the code has to agree with it even on the
        // combination nothing constructs.
        let one = Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Node {
                aim: Aim::Ticket {
                    number: 16,
                    title: "the ticket".to_string(),
                    ticket_type: TicketType::Build,
                    stage: Launchable::Ready,
                    prs: vec![],
                },
                map: map_ref(1),
                route: Route::One,
                mode: interactive(""),
            },
            isolation: Isolation::Host,
        };
        assert_eq!(one.agent_argv().last().map(String::as_str), Some("/wf-one"));
    }

    #[test]
    fn a_map_launch_is_the_skill_and_the_map_number_alone() {
        // No ticket argument exists to pass, so none is passed — the map aim
        // is the whole subject (#96).
        assert_eq!(elided(&map_prompt(&interactive(""))), "/wf 59 ctx: …");
        assert_eq!(elided(&map_prompt(&auto(""))), "/wf-auto 59 ctx: …");
        assert_eq!(
            elided(&map_prompt(&auto("merge when green"))),
            "/wf-auto 59 ctx: … steer: merge when green"
        );
        // A map's key is its own issue number, not a ticket's.
        let staged = Staged::map(&map_ref(59));
        assert_eq!(staged.key(), "#59");
        match plan_picked(&cache(), &staged, &interactive("")) {
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
            title: "the ticket".to_string(),
            ticket_type,
            stage,
            prs: vec![],
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
    fn mid_routes_every_node_to_wayfinder_mid() {
        // `mid` collapses the table for `auto`'s reason: the launched session
        // manages whatever the node needs — settling what the principles
        // settle, asking where they don't, and handing a build's stages to
        // `wf-tdd` and `wf-review` itself. Routing a build node to its stage
        // skill instead would draw two picker rows naming the same skill,
        // which is the one thing a per-row route exists to prevent.
        for ticket_type in DECISION_TYPES.into_iter().chain([TicketType::Build]) {
            for stage in LAUNCHABLE {
                assert_eq!(
                    route(&aim(ticket_type, stage), Mode::Mid),
                    Route::WayfinderMid,
                    "{ticket_type:?} at {stage:?}"
                );
            }
        }
        assert_eq!(route(&Aim::Map, Mode::Mid), Route::WayfinderMid);
    }

    #[test]
    fn the_mid_route_names_the_wf_mid_skill() {
        // The mode is a *skill*, not a flag on one, so the route has to name a
        // prompt the package carries — the sweep over `Route::all` is what
        // fails the build if it does not.
        assert_eq!(Route::WayfinderMid.label(), "/wf-mid");
        assert_eq!(Route::WayfinderMid.bundled_skill(), Some("wf-mid"));
        assert!(
            Route::all().contains(&Route::WayfinderMid),
            "reachable in the cycle"
        );
    }

    #[test]
    fn plain_launches_a_session_with_no_skill_in_it() {
        // The last mode collapses the table the way `auto` and `mid` do, and
        // for the opposite reason: they pick one skill for every node, `plain`
        // picks none. Which node it was aimed at cannot change that, so every
        // cell answers the same.
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
            vec![
                "claude".to_string(),
                Agent::Claude.skip_permissions().to_string()
            ]
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
                Agent::Claude.skip_permissions().to_string(),
                "rebase onto main".to_string()
            ]
        );
        assert_eq!(
            map_argv(&plain("what is actually left in here?")),
            vec![
                "claude".to_string(),
                Agent::Claude.skip_permissions().to_string(),
                "what is actually left in here?".to_string()
            ]
        );
        assert_eq!(
            map_argv(&plain("")),
            vec![
                "claude".to_string(),
                Agent::Claude.skip_permissions().to_string()
            ]
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
                Staged::ticket(&node, &map_ref(1), Stage::Done),
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
    fn a_dl_version_is_read_from_either_install_and_from_nothing_else() {
        // The released build, and the editable dev install whose trailer names
        // the tree it resolves to. Both are versions `wf` must be able to
        // compare — refusing the second would refuse isolation to the person
        // developing `dl`.
        assert_eq!(DlVersion::parse("dl 0.0.24\n"), Some(DlVersion(0, 0, 24)));
        assert_eq!(
            DlVersion::parse("dl 0.0.24 (dev, editable from /home/x/projects/devlaunch)\n"),
            Some(DlVersion(0, 0, 24))
        );

        // A pre-release tail belongs to the version that carries the feature.
        assert_eq!(DlVersion::parse("dl 0.0.24rc1"), Some(DlVersion(0, 0, 24)));

        // Multi-digit components are not parsed a character at a time.
        assert_eq!(DlVersion::parse("dl 1.12.30"), Some(DlVersion(1, 12, 30)));

        // Nothing to compare: no output, no number, and a number too short to
        // be a version. Each has to be `None` rather than a zero, because
        // `0.0.0` would read as "older than the floor" — a wrong reason.
        assert_eq!(DlVersion::parse(""), None);
        assert_eq!(DlVersion::parse("dl: command not found"), None);
        assert_eq!(DlVersion::parse("dl 0.1"), None);
    }

    #[test]
    fn the_floor_separates_a_dl_that_speaks_this_binarys_command_line_from_one_that_does_not() {
        // 0.0.23 is the release that had every other condition right and no
        // `up` subcommand — the case this floor exists for.
        assert_eq!(
            Devlaunch::from_version_output("dl 0.0.23"),
            Devlaunch::TooOld(DlVersion(0, 0, 23))
        );
        assert_eq!(
            Devlaunch::from_version_output("dl 0.0.24"),
            Devlaunch::Usable
        );
        // Everything above the floor stays usable, including across the
        // components a string comparison would get wrong.
        assert_eq!(
            Devlaunch::from_version_output("dl 0.0.100"),
            Devlaunch::Usable
        );
        assert_eq!(
            Devlaunch::from_version_output("dl 0.1.0"),
            Devlaunch::Usable
        );
        assert_eq!(
            Devlaunch::from_version_output("dl 1.0.0"),
            Devlaunch::Usable
        );
        // Unreadable, not usable: a `dl` whose answer cannot be compared is a
        // `dl` whose command line cannot be relied on.
        assert_eq!(Devlaunch::from_version_output("???"), Devlaunch::Unreadable);
    }

    #[test]
    fn the_unsaved_reading_follows_the_release_that_changed_it_not_the_floor() {
        // Usable is the ordinary yes. It is only sound because the floor is at
        // or above the release that changed the field — assert that, so raising
        // the floor below it can never make `Usable` claim an answer that
        // release did not give.
        assert!(DEVLAUNCH_FLOOR >= UNSAVED_IS_AN_OBJECT);

        // The case the two constants exist for: a `dl` below a future floor
        // that still answers `unsaved`. Read as answering, because it does.
        assert!(Devlaunch::TooOld(DlVersion(0, 0, 24)).answers_unsaved());
        assert!(Devlaunch::TooOld(DlVersion(0, 1, 0)).answers_unsaved());

        // 0.0.23 is the release whose `null` meant clean. Reading it as an
        // unanswered question would refuse every workspace on that machine.
        assert!(!Devlaunch::TooOld(DlVersion(0, 0, 23)).answers_unsaved());

        // No version to compare: read the old, permissive way rather than
        // refusing everything.
        assert!(!Devlaunch::Absent.answers_unsaved());
        assert!(!Devlaunch::Unreadable.answers_unsaved());
        assert!(Devlaunch::Usable.answers_unsaved());
    }

    #[test]
    fn only_a_dl_that_could_have_worked_puts_a_reason_in_the_notice() {
        // Installed and unusable is the fixable state, and the one that reads
        // as `wf` ignoring the devcontainer if it says nothing.
        let too_old = Devlaunch::TooOld(DlVersion(0, 0, 23))
            .shortfall()
            .expect("a reason");
        assert!(too_old.contains("dl 0.0.23"), "{too_old}");
        assert!(too_old.contains("0.0.24"), "names the floor: {too_old}");
        assert!(Devlaunch::Unreadable.shortfall().is_some());

        // Silent: nothing to fix, or nothing the user asked for.
        assert_eq!(Devlaunch::Usable.shortfall(), None);
        assert_eq!(Devlaunch::Absent.shortfall(), None);
    }

    #[test]
    fn codex_stays_on_the_host_even_when_the_checkout_declares_a_devcontainer() {
        // `dl` presently hands a container Claude's config tree, not Codex's.
        // A host launch can read the Codex skill copies that `wf skills install`
        // made; an isolated one would only fail after replacing the picker.
        let checkout = Scratch::new("codex-host");
        checkout.touch(".devcontainer/devcontainer.json");
        assert_eq!(
            Isolation::detect(&checkout.0, Agent::Codex),
            Isolation::Host
        );
    }

    #[test]
    fn a_checkout_without_a_devcontainer_runs_on_the_host_whatever_is_on_path() {
        // The other half — a devcontainer but no `dl` — is the degradation
        // path, and it is what every other test in this module exercises by
        // running on a machine that may or may not have `dl`. This direction is
        // the one that must hold unconditionally.
        let bare = Scratch::new("host");
        assert_eq!(Isolation::detect(&bare.0, Agent::Claude), Isolation::Host);
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
                    title: "the ticket".to_string(),
                    ticket_type: TicketType::Task,
                    stage: Launchable::Ready,
                    prs: vec![],
                },
                map: map_ref(67),
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
            job: Job::Create {
                creation,
                agent: Agent::Claude,
            },
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
            elided_argv(&isolated(Route::Wayfinder, interactive(""))),
            vec![
                "dl".to_string(),
                "blooop/wayfinder@wayfinder/wayfinder-80".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions' '/wf 67 80 ctx: …'".to_string(),
            ]
        );
        // The steering suffix rides inside the same quoted argument, after the
        // context block.
        assert_eq!(
            elided_argv(&isolated(Route::WayfinderAuto, auto("merge when green")))[3],
            "'claude' '--dangerously-skip-permissions' \
             '/wf-auto 67 80 ctx: … steer: merge when green'"
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
    fn the_prewarm_names_the_same_workspace_the_launch_will_look_for() {
        // The whole point of warming at stage time: the container the prewarm
        // builds must be the one the second enter's `dl` finds. One naming
        // function serves both, and this pins that they cannot drift.
        let node = ticket("blooop/wayfinder", 80);
        let staged = Staged::ticket(&node, &map_ref(67), Stage::Ready).expect("launchable");
        assert_eq!(
            staged.node_workspace().as_deref(),
            Some(isolated_ticket(80, Route::Tdd, interactive("")).agent_argv()[1].as_str())
        );
        // A staged map warms the map's own node, same as launching it.
        let map = Staged::map(&MapRef::new(
            &MapId::new("blooop/wayfinder", 67),
            "the tree",
        ));
        assert_eq!(
            map.node_workspace().as_deref(),
            Some("blooop/wayfinder@wayfinder/wayfinder-67")
        );
    }

    #[test]
    fn the_map_less_door_names_no_workspace_to_warm() {
        // It offers creation rows alone (#114): no node, so nothing a launch
        // would attach to, so nothing to warm. A creation resolves to the
        // repo's bare default workspace, which staging must not pre-build on
        // the strength of a keystroke.
        let door = Staged::project("blooop/wayfinder");
        assert_eq!(door.node_workspace(), None);
        assert_eq!(prewarm(&cache(), &door), None);
    }

    #[test]
    fn the_prewarm_is_off_until_it_is_asked_for() {
        // Staging must create nothing for anyone who has not opted in.
        assert!(!enabled_from(None));
        for off in ["", "0", "false", "no", "FALSE", " No "] {
            assert!(!enabled_from(Some(off)), "{off:?} means off");
        }
        for on in ["1", "true", "yes", "on", "YES", " 1 "] {
            assert!(enabled_from(Some(on)), "{on:?} means on");
        }
        // The allowlist earns its keep here: a value nobody anticipated must
        // not start building containers. Under `dl`'s opposite rule — which
        // is right for an opt-out — every one of these would enable it.
        for unrecognised in ["off", "disabled", "none", "nope", "0.0", "never"] {
            assert!(
                !enabled_from(Some(unrecognised)),
                "{unrecognised:?} must not turn the prewarm on"
            );
        }
    }

    #[test]
    fn a_host_only_repo_plans_no_prewarm() {
        // The cache's paths do not exist, so no candidate can detect a
        // devcontainer: there is no container to warm, and no `dl` is spawned
        // for a launch that will run on the host. An unregistered repo warms
        // nothing either — there is nothing to launch into at all.
        let node = ticket("blooop/wayfinder", 80);
        let staged = Staged::ticket(&node, &map_ref(67), Stage::Ready).expect("launchable");
        assert_eq!(prewarm(&cache(), &staged), None);
        assert_eq!(prewarm(&[], &staged), None);
    }

    /// A handoff with both stamps chosen, rather than read off this machine's
    /// clock: the values below are literals, so what the seam *spells* is
    /// checkable and not merely self-consistent.
    fn handoff(t0: SystemTime, prewarm_fired: Option<SystemTime>) -> Handoff {
        Handoff { t0, prewarm_fired }
    }

    /// A wall-clock instant, epoch seconds and nanoseconds.
    fn instant(secs: u64, nanos: u32) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos)
    }

    /// The same launch on the host — the arm that execs the agent directly,
    /// with no `dl` between `wf` and it.
    fn on_the_host(route: Route, mode: LaunchMode) -> Launch {
        Launch {
            isolation: Isolation::Host,
            ..isolated(route, mode)
        }
    }

    #[test]
    fn the_launch_hands_dl_the_keystroke_it_resolved_from() {
        // The variable names and the spelling of the value are `dl`'s to mint
        // (blooop/devlaunch#194) and are quoted here as the literals they are:
        // Unix epoch seconds, exactly what `date +%s.%N` prints, so a reader
        // that parses one parses the other.
        assert_eq!(
            isolated(Route::Tdd, interactive(""))
                .stamps(&handoff(instant(1_755_194_037, 123_456_789), None)),
            [
                (
                    "DEVLAUNCH_HANDOFF_T0",
                    Some("1755194037.123456789".to_string())
                ),
                ("DEVLAUNCH_PREWARM_FIRED_AT", None),
            ],
            "the keystroke travels; a prewarm nobody fired does not"
        );
    }

    #[test]
    fn a_prewarm_that_fired_travels_beside_it_as_the_instant_it_fired() {
        // The second variable carries *when `wf` fired*, never how the launch
        // turned out: hit, partial and miss are `dl`'s to observe from the arm
        // it takes, and a stamp that claimed one would be `wf` reporting on a
        // container it never saw.
        let stamps = isolated(Route::Tdd, interactive("")).stamps(&handoff(
            instant(1_755_194_037, 500_000_000),
            Some(instant(1_755_194_030, 0)),
        ));
        assert_eq!(
            stamps,
            [
                (
                    "DEVLAUNCH_HANDOFF_T0",
                    Some("1755194037.500000000".to_string())
                ),
                (
                    "DEVLAUNCH_PREWARM_FIRED_AT",
                    Some("1755194030.000000000".to_string())
                ),
            ]
        );
        for (var, value) in stamps {
            let value = value.expect("both stamps are set here");
            assert!(
                value.parse::<f64>().is_ok(),
                "{var} carries an instant and nothing else, not {value:?}"
            );
        }
    }

    #[test]
    fn a_launch_that_is_not_a_handover_to_dl_carries_no_stamp() {
        // The seam is `wf` → `dl`, and a host launch execs the agent itself:
        // there is no reader for a stamp there, and one left in an agent's
        // environment would be read by every unrelated `dl` that session goes
        // on to run — a handoff measured from a keystroke hours old. `None` is
        // *removed*, not merely unset, so an inherited stamp cannot ride out
        // on a launch that never minted one.
        assert_eq!(
            on_the_host(Route::Tdd, interactive("")).stamps(&handoff(
                instant(1_755_194_037, 0),
                Some(instant(1_755_194_030, 0))
            )),
            [
                ("DEVLAUNCH_HANDOFF_T0", None),
                ("DEVLAUNCH_PREWARM_FIRED_AT", None),
            ]
        );
    }

    #[test]
    fn a_dl_wf_starts_and_does_not_become_clears_the_stamps_it_inherited() {
        // The other half of the rule above, and the one a clean environment
        // hides: the probe, the prewarm's `dl <ws> up` and `wf reap`'s listing
        // and removals are children, not execs, so they inherit `wf`'s
        // environment — and `wf` run inside a workspace has both stamps in it
        // already, set by the `dl` that launched the agent running it. Every
        // one of those children would then report a hand-over from a keystroke
        // in another session, which the reader cannot tell from a real one.
        //
        // Asserted on the command as built rather than on a child's
        // environment, because the machine running this need not have a `dl`
        // at all; the end-to-end version is claim 6 of
        // `tests/live_launch_exec.rs`, which starts `wf` with both already set.
        let command = unstamped(DEVLAUNCH);
        let changed: BTreeSet<(&str, Option<&str>)> = command
            .get_envs()
            .map(|(var, value)| {
                (
                    var.to_str().expect("ascii"),
                    value.map(|v| v.to_str().expect("ascii")),
                )
            })
            .collect();
        // The whole environment delta, not merely "the two are in it": a `None`
        // is a removal and a `Some` is a stamp, so this is also the assertion
        // that no child of `wf`'s is quietly *given* one.
        let expected: BTreeSet<(&str, Option<&str>)> = Handoff::variables()
            .into_iter()
            .map(|var| (var, None))
            .collect();
        assert_eq!(
            changed, expected,
            "a `dl` that is not the launch clears both stamps and sets neither"
        );
    }

    #[test]
    fn the_seam_owns_exactly_two_variables() {
        // The published list, which the docs guard reads: two, in the order
        // they are documented, and every stamp a launch sets is one of them.
        assert_eq!(
            Handoff::variables(),
            ["DEVLAUNCH_HANDOFF_T0", "DEVLAUNCH_PREWARM_FIRED_AT"]
        );
        let set: Vec<&str> = isolated(Route::Tdd, interactive(""))
            .stamps(&handoff(instant(1_755_194_037, 0), None))
            .into_iter()
            .map(|(var, _)| var)
            .collect();
        assert_eq!(set, Handoff::variables());
    }

    #[test]
    fn the_keystroke_is_stamped_when_the_handoff_is_taken() {
        // `now` is the whole production constructor: there is no way to build a
        // handoff carrying an instant that is not this machine's clock at the
        // moment it was taken.
        let before = SystemTime::now();
        let taken = Handoff::now(None);
        let after = SystemTime::now();
        assert!(taken.t0 >= before && taken.t0 <= after);
        assert_eq!(taken.prewarm_fired, None);
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
                map: map_ref(67),
                route: Route::WayfinderAuto,
                mode: auto(""),
            },
            isolation: Isolation::Devlaunch,
        };
        assert_eq!(
            elided_argv(&launch),
            vec![
                "dl".to_string(),
                "blooop/wayfinder@wayfinder/wayfinder-67".to_string(),
                "--".to_string(),
                "'claude' '--dangerously-skip-permissions' '/wf-auto 67 ctx: …'".to_string(),
            ]
        );
    }

    #[test]
    fn steering_text_cannot_break_out_of_the_shell_command() {
        // The one string a user types that reaches a shell. A single quote
        // closes, escapes, reopens — the argument stays one argument.
        let launch = isolated(Route::Tdd, interactive("don't touch the CI; rm -rf /"));
        assert_eq!(
            elided_argv(&launch)[3],
            r"'claude' '--dangerously-skip-permissions' '/wf-tdd 80 ctx: … steer: don'\''t touch the CI; rm -rf /'"
        );
        // Every metacharacter a shell would otherwise act on is inside quotes.
        assert_eq!(shell_quote("a b|c;d&e$f`g"), "'a b|c;d&e$f`g'");
        assert_eq!(shell_quote(""), "''");
    }

    /// The same node launched both ways — into a container and on the host —
    /// so the container seam can be asserted against what the host's agent is
    /// handed, rather than against a second spelling of it.
    fn both_ways(title: &str, route: Route, mode: LaunchMode) -> (Launch, Launch) {
        let job = Job::Node {
            aim: Aim::Ticket {
                number: 80,
                title: title.to_string(),
                ticket_type: TicketType::Task,
                stage: Launchable::Ready,
                prs: vec![],
            },
            map: map_ref(67),
            route,
            mode,
        };
        let built = |isolation| Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: job.clone(),
            isolation,
        };
        (built(Isolation::Devlaunch), built(Isolation::Host))
    }

    /// Names the scratch directory each recovery runs in, so two calls in one
    /// test process cannot read each other's leavings.
    static NEXT_SCRATCH: AtomicUsize = AtomicUsize::new(0);

    /// The argument vector a container would actually run, recovered by giving
    /// the single shell command `dl` passes to `devpod ssh --command` to a
    /// **real POSIX shell**.
    ///
    /// A hand-written inverse of [`shell_quote`] would only prove this module
    /// agrees with itself; `sh` is the thing on the other side of the seam, so
    /// it is what does the unquoting here. `set --` performs exactly the word
    /// splitting and quote removal a command line gets, without running the
    /// agent, and NUL separation keeps a recovered argument's own spaces from
    /// being mistaken for a boundary.
    ///
    /// The shell runs in a scratch directory of its own, and the directory is
    /// asserted empty afterwards. The fixture title carries `$(touch pwned)`
    /// precisely so that a broken [`shell_quote`] is caught by *evidence the
    /// substitution ran* rather than by an argv mismatch alone — but a canary
    /// dropped in whatever directory `cargo test` happened to start in is
    /// litter, and once got committed to this public repo. Relative to the
    /// shell's own cwd it lands here instead, where the emptiness check reads
    /// it: the canary got stronger (nothing asserted on it before) and stopped
    /// writing outside its own scratch.
    fn container_argv(launch: &Launch) -> Vec<String> {
        let argv = launch.agent_argv();
        assert_eq!(argv[0], DEVLAUNCH, "the isolated form is a `dl` launch");
        assert_eq!(argv[2], "--", "the agent command follows a bare `--`");
        let scratch = std::env::temp_dir().join(format!(
            "wf-seam-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        recovered_in(&scratch, &argv[3])
    }

    /// The recovery itself, in a scratch directory named by the caller —
    /// created here, and removed here on **every** exit, the refusal panic
    /// included: a long-lived host must not accumulate `wf-seam-*` litter
    /// because one command was refused (#133).
    fn recovered_in(scratch: &Path, command: &str) -> Vec<String> {
        std::fs::create_dir_all(scratch).expect("a scratch directory for the shell");
        let script = format!("set -- {command}\nfor arg; do printf '%s\\0' \"$arg\"; done");
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .current_dir(scratch)
            .output()
            .expect("a POSIX shell");
        let spilled: Vec<_> = std::fs::read_dir(scratch)
            .expect("the scratch directory outlives the shell")
            .map(|entry| entry.expect("a readable entry").file_name())
            .collect();
        std::fs::remove_dir_all(scratch).expect("the scratch directory is ours to remove");
        assert!(
            out.status.success(),
            "the container's shell refused the command {command:?}"
        );
        assert!(
            spilled.is_empty(),
            "the shell executed something the quoting should have made inert, \
             leaving {spilled:?} behind"
        );
        let recovered = String::from_utf8(out.stdout).expect("the arguments are utf-8");
        let mut words: Vec<String> = recovered.split('\0').map(str::to_string).collect();
        assert_eq!(
            words.pop().as_deref(),
            Some(""),
            "every argument is NUL-terminated"
        );
        words
    }

    #[test]
    fn a_refused_command_leaves_no_scratch_behind() {
        // The normal path already asserts its scratch is empty and removes
        // it; this is the other path. A refused command panics — that is the
        // canary doing its job — but the panic must not be the reason a
        // long-lived host collects `wf-seam-*` directories in $TMPDIR.
        let scratch = std::env::temp_dir().join(format!("wf-seam-{}-refused", std::process::id()));
        let refused = std::panic::catch_unwind(|| recovered_in(&scratch, "'unterminated"));
        assert!(refused.is_err(), "an unterminated quote is refused by sh");
        assert!(
            !scratch.exists(),
            "the refusal path must clean its scratch too: {scratch:?}"
        );
    }

    #[test]
    fn the_containers_own_shell_hands_the_agent_what_the_host_would() {
        // The seam the ticket insists on: an isolated launch's prompt is not
        // an argv entry by the time it arrives — `dl` joins everything after
        // `--` and hands one string to `devpod ssh --command`, which a shell
        // inside the container parses. So the claim is that the shell rebuilds
        // the context block byte for byte, with a title carrying every
        // character that would end the argument early if the quoting were
        // wrong: a single quote, a command substitution and a double quote.
        let (contained, host) = both_ways(
            r#"don't $(touch pwned) "x""#,
            Route::Tdd,
            interactive("merge when green"),
        );
        assert_eq!(container_argv(&contained), host.agent_argv());
        assert_eq!(
            host.agent_argv(),
            vec![
                "claude".to_string(),
                Agent::Claude.skip_permissions().to_string(),
                concat!(
                    r#"/wf-tdd 80 ctx: {"v":1,"repo":"blooop/wayfinder","#,
                    r#""map":{"repo":"blooop/wayfinder","number":67,"title":"the dev-process tree"},"#,
                    r#""aim":{"ticket":{"number":80,"title":"don't $(touch pwned) \"x\"","#,
                    r#""ticket_type":"task","stage":"ready","prs":[]}}} steer: merge when green"#
                )
                .to_string(),
            ]
        );
    }

    #[test]
    fn a_map_launch_survives_the_container_seam_too() {
        // The other aim, and the plain session that carries no block at all:
        // both have to come back out of the shell as they went in.
        let (contained, host) = both_ways("the ticket", Route::Plain, plain("look around"));
        assert_eq!(container_argv(&contained), host.agent_argv());
        let map = Launch {
            repo: "blooop/wayfinder".to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            job: Job::Node {
                aim: Aim::Map,
                map: map_ref(67),
                route: Route::WayfinderAuto,
                mode: auto(""),
            },
            isolation: Isolation::Devlaunch,
        };
        assert_eq!(
            container_argv(&map).last().expect("a prompt"),
            concat!(
                r#"/wf-auto 67 ctx: {"v":1,"repo":"blooop/wayfinder","#,
                r#""map":{"repo":"blooop/wayfinder","number":67,"title":"the dev-process tree"},"#,
                r#""aim":"map"}"#
            )
        );
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

    /// A scratch directory of PATH entries, removed on drop. Real files rather
    /// than a mock, because the question under test — would the OS execute
    /// this candidate? — is a fact about an inode's mode bits.
    struct FakePath(PathBuf);

    impl FakePath {
        fn new(name: &str) -> FakePath {
            let dir = std::env::temp_dir().join(format!("wf-launch-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            FakePath(dir)
        }

        /// One PATH entry holding one file named `program`, with `mode`.
        fn entry(&self, dir: &str, program: &str, mode: u32) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let entry = self.0.join(dir);
            std::fs::create_dir_all(&entry).expect("entry");
            let file = entry.join(program);
            std::fs::write(&file, "#!/bin/sh\n").expect("candidate");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))
                .expect("mode");
            entry
        }
    }

    impl Drop for FakePath {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn path_resolution_skips_a_candidate_the_os_would_not_execute() {
        // execvp keeps searching past a file it cannot execute; a stray
        // non-executable `claude` early on PATH must not fail a launch the OS
        // would have made.
        let scratch = FakePath::new("skips-non-executable");
        let decoy = scratch.entry("decoy", "agent", 0o644);
        let real = scratch.entry("real", "agent", 0o755);
        let path = std::env::join_paths([&decoy, &real]).expect("a PATH");
        let resolved = resolve_in("agent", &path).expect("the executable is on PATH");
        assert_eq!(resolved, real.join("agent"));
    }

    #[test]
    fn a_name_found_only_as_a_non_executable_file_is_not_on_path() {
        let scratch = FakePath::new("only-non-executable");
        let decoy = scratch.entry("decoy", "agent", 0o644);
        let path = std::env::join_paths([&decoy]).expect("a PATH");
        assert!(resolve_in("agent", &path).is_err());
    }

    #[tokio::test]
    async fn a_blocking_call_through_the_probe_seam_does_not_stall_the_runtime() {
        // `#[tokio::test]` is a current-thread runtime, so if the blocking
        // work ran on the worker, the timer spawned here could not fire until
        // it finished — and the flag it sets would still be unset when the
        // work reads it. The margins are 20ms against 300ms, so a slow CI
        // machine moves both the same way rather than flipping the verdict.
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let ticked = Arc::new(AtomicBool::new(false));
        let for_timer = Arc::clone(&ticked);
        let timer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            for_timer.store(true, Ordering::SeqCst);
        });
        let for_work = Arc::clone(&ticked);
        let timer_fired_while_blocked = off_runtime(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            for_work.load(Ordering::SeqCst)
        })
        .await;
        timer.await.expect("the timer task completes");
        assert!(
            timer_fired_while_blocked,
            "the runtime's timers stood still while the blocking call ran — \
             the probe is stalling the worker thread"
        );
    }
}
