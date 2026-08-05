//! The launch seam (Build 4, #16) — the daily-use line.
//!
//! One picked ticket becomes a zellij tab in that project's session, cwd set
//! to the checkout, running the `/wayfinder` skill. The tab has two names and
//! the difference is load-bearing (#20): its **key** [`TabKey`] —
//! `<short_repo>#<number>`, per the #7 naming amendment — is its identity, and
//! its **label** [`TabLabel`] is the key plus a capped title, which is what a
//! human reads in the tab strip. Only the label reaches `new-tab --name`; only
//! the key is ever looked up, so retitling an issue cannot make a ticket miss
//! its own tab. Topology and semantics come
//! from the #5 resolution: session per project, tab per ticket, create or
//! *focus* by name, no `--close-on-exit` (an EXITED tab is the post-mortem),
//! and no new navigation keybindings — HITL hands the terminal over by
//! running `zellij attach` as a **child** process so detaching returns to
//! `wf`.
//!
//! The prototype's guards (#5 findings) are load-bearing here:
//!
//! * `zellij action` exits **0 even when it did nothing**, so nothing in
//!   this module trusts an exit code. Success is verified by re-reading
//!   `query-tab-names` / `list-sessions`.
//! * `ZELLIJ_SESSION_NAME` naming a dead session makes `zellij` **hang
//!   forever**, so every invocation scrubs the inherited zellij variables
//!   and names its target session explicitly with `--session`.
//! * Where `wf` itself runs is decided from `wf`'s own `$ZELLIJ`
//!   ([`detect_host`]), never inferred from zellij's output.
//! * A killed session is serialized and would be *resurrected* stale, so a
//!   session found EXITED is `delete-session`d before being recreated.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::model::Ticket;
use crate::projects::Checkout;

/// How the agent runs — the one difference between a HITL and an AFK launch
/// (#7: an AFK agent is the same tab through the same seam, minus the
/// attach and minus the focus steal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Human in the loop: interactive `claude`, and `wf` hands over the
    /// terminal (or the zellij client) to it.
    Hitl,
    /// Away from keyboard: headless `claude -p`, spawned and left alone.
    Afk,
}

impl Mode {
    /// Short label for notices and the picker.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Hitl => "session",
            Mode::Afk => "afk agent",
        }
    }
}

/// Where `wf` itself is running. Read once from `wf`'s own environment —
/// never from a `zellij action` exit code, which is 0 even on failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// No zellij session owns this terminal: `wf` must suspend itself and
    /// run `zellij attach` as a child to hand over.
    Outside,
    /// Inside this zellij session: native navigation moves the client, and
    /// `wf` keeps running in its own tab.
    Inside(String),
    /// `$ZELLIJ` is set but nameless (`$ZELLIJ_SESSION_NAME` missing). The
    /// tab is still created; `wf` just cannot move the client itself.
    InsideUnnamed,
}

/// Classify the launch host from the two zellij variables.
pub fn host_from_env(zellij: Option<&str>, session_name: Option<&str>) -> Host {
    match zellij {
        None => Host::Outside,
        Some(_) => match session_name.map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => Host::Inside(name.to_string()),
            None => Host::InsideUnnamed,
        },
    }
}

/// [`host_from_env`] against the live process environment.
pub fn detect_host() -> Host {
    let zellij = std::env::var("ZELLIJ").ok();
    let name = std::env::var("ZELLIJ_SESSION_NAME").ok();
    host_from_env(zellij.as_deref(), name.as_deref())
}

/// A fully-resolved launch: which session hosts the tab, what the tab is
/// called, where it runs, and what it runs. Constructed only by [`plan`],
/// so a launch whose checkout belongs to a different repo than its ticket
/// is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub mode: Mode,
    /// The project's zellij session, read from the projects cache at launch
    /// time (it is recomputed on every registration — #15).
    pub session: String,
    /// The checkout the agent works in: the tab's cwd.
    pub cwd: PathBuf,
    /// What the tab is called. Stored as the *label* because a label always
    /// contains its key ([`Launch::key`]), never the other way round — so no
    /// caller can reach for the mutable half when it wanted the stable one.
    pub label: TabLabel,
    /// The repo's map issue — the first argument to `/wayfinder`.
    pub map_issue: u64,
    /// The ticket being worked.
    pub ticket: u64,
}

/// A ticket's tab **identity**: `<short_repo>#<number>` (#7 amendment) — the
/// same key the picker's rows show, so tab strip and picker share one identity.
///
/// Deliberately cannot hold a title: this is what focus-or-create looks up and
/// what [`is_agent_tab`] recognises, so it must be a function of the ticket's
/// identity alone. Retitling issue 20 leaves `wayfinder#20` unchanged, which is
/// the whole point (#20).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabKey {
    repo: String,
    number: u64,
}

impl TabKey {
    /// Recover a key from a zellij tab name — the parse at the zellij
    /// boundary. Tolerates everything a displayed name may carry *after* the
    /// key: a label's title, an activity marker, or both.
    pub fn parse(tab_name: &str) -> Option<TabKey> {
        let head = tab_name.trim().split(' ').next()?;
        let (repo, number) = head.split_once('#')?;
        // Digits only: `u64::from_str` would also take `+16`, and a tab named
        // `wayfinder#+16` is not one of ours.
        if repo.is_empty() || number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(TabKey {
            repo: repo.to_string(),
            number: number.parse().ok()?,
        })
    }

    /// The short repo name (display half of the slug — see [`Ticket::short_repo`]).
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The ticket number.
    pub fn number(&self) -> u64 {
        self.number
    }
}

impl fmt::Display for TabKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

/// How many characters of a ticket's title a label may carry. Zellij truncates
/// the tab strip anyway, so a name that only reads well untruncated does not
/// read at all (#20).
const TITLE_CAP: usize = 18;

/// A ticket's tab **label**: its key plus a capped short title — what a human
/// reads in the tab strip, and the *only* string that reaches
/// `new-tab --name`.
///
/// Built from a [`TabKey`] rather than from a string, so "a label starts with
/// its key" is an invariant of the type instead of a comment, and
/// [`TabLabel::key`] is total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLabel {
    key: TabKey,
    /// `None` for a ticket whose title is empty once normalised — then the
    /// label simply *is* the key. Not a sentinel: both cases render.
    short_title: Option<String>,
}

impl TabLabel {
    /// The stable identity inside this label. The only bridge between the two,
    /// and it only runs this way: a key can never be widened into a label.
    pub fn key(&self) -> &TabKey {
        &self.key
    }

    /// The capped title, if the ticket had one.
    pub fn short_title(&self) -> Option<&str> {
        self.short_title.as_deref()
    }
}

impl fmt::Display for TabLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.short_title {
            Some(title) => write!(f, "{} {}", self.key, title),
            None => write!(f, "{}", self.key),
        }
    }
}

/// The stable tab identity for a ticket.
pub fn tab_key(ticket: &Ticket) -> TabKey {
    TabKey {
        repo: ticket.short_repo().to_string(),
        number: ticket.number,
    }
}

/// The readable tab name for a ticket: key + capped title.
pub fn tab_label(ticket: &Ticket) -> TabLabel {
    TabLabel {
        key: tab_key(ticket),
        short_title: short_title(&ticket.title),
    }
}

/// Normalise and cap a ticket title for a tab label: whitespace collapsed to
/// single spaces (a tab name is one line), control characters dropped, then cut
/// to [`TITLE_CAP`] characters **on a word boundary** with `…` marking the cut.
/// A first word longer than the cap is broken mid-word — better a stub than
/// nothing.
fn short_title(title: &str) -> Option<String> {
    let words: Vec<String> = title
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    let full = words.join(" ");
    if full.is_empty() {
        return None;
    }
    if full.chars().count() <= TITLE_CAP {
        return Some(full);
    }
    let mut capped = String::new();
    for word in &words {
        let extra = word.chars().count() + usize::from(!capped.is_empty());
        if capped.chars().count() + extra > TITLE_CAP {
            break;
        }
        if !capped.is_empty() {
            capped.push(' ');
        }
        capped.push_str(word);
    }
    if capped.is_empty() {
        // One long word: no boundary to break on, so break inside it.
        capped = full.chars().take(TITLE_CAP).collect();
    }
    capped.push('…');
    Some(capped)
}

/// The checkouts that could host a ticket's tab: every registered checkout
/// of the ticket's repo, matched on the **full** slug (a fork and its
/// upstream share a short name — #15). Cache order (sorted by path) is
/// preserved so the picker is stable.
pub fn candidate_checkouts<'a>(checkouts: &'a [Checkout], repo: &str) -> Vec<&'a Checkout> {
    checkouts.iter().filter(|c| c.repo == repo).collect()
}

/// What a launch request resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Targets {
    /// No registered checkout of this repo on this machine — nothing to
    /// launch into. (Only reachable if the cache changed under us: map
    /// tickets exist because a cached checkout named their repo.)
    Unregistered,
    /// Exactly one candidate: launch straight away, no prompt.
    One(Launch),
    /// Several checkouts of one repo (the k1–k5 pattern): the human picks
    /// which project hosts the tab.
    Many(Vec<Launch>),
}

/// Resolve a launch request against the projects cache. Zero or one
/// candidate never prompts.
pub fn plan(checkouts: &[Checkout], ticket: &Ticket, map_issue: u64, mode: Mode) -> Targets {
    let label = tab_label(ticket);
    let launches: Vec<Launch> = candidate_checkouts(checkouts, &ticket.repo)
        .into_iter()
        .map(|c| Launch {
            mode,
            session: c.session.clone(),
            cwd: c.path.clone(),
            label: label.clone(),
            map_issue,
            ticket: ticket.number,
        })
        .collect();
    match launches.len() {
        0 => Targets::Unregistered,
        1 => Targets::One(launches.into_iter().next().expect("len checked")),
        _ => Targets::Many(launches),
    }
}

/// The `/wayfinder` invocation as one prompt argument — `claude` takes a
/// single positional prompt, so the slash command and its arguments must
/// not be split across argv entries.
fn prompt(map_issue: u64, ticket: u64) -> String {
    format!("/wayfinder {map_issue} {ticket}")
}

impl Launch {
    /// This launch's stable tab identity — what every lookup uses.
    pub fn key(&self) -> &TabKey {
        self.label.key()
    }

    /// The command the tab runs: interactive `claude` for HITL, headless
    /// `claude -p` for AFK (#7).
    pub fn agent_argv(&self) -> Vec<String> {
        let prompt = prompt(self.map_issue, self.ticket);
        match self.mode {
            Mode::Hitl => vec!["claude".to_string(), prompt],
            Mode::Afk => vec!["claude".to_string(), "-p".to_string(), prompt],
        }
    }

    /// The invocation that creates this launch's tab.
    pub fn new_tab_argv(&self) -> Vec<String> {
        new_tab_argv(&self.session, &self.label, &self.cwd, &self.agent_argv())
    }

    /// One-line description for notices and the picker. The **key**, not the
    /// label: notices are about identity and share a line with other chrome.
    pub fn describe(&self) -> String {
        format!("{} in {}", self.key(), self.session)
    }
}

/// `zellij --session <session> action <args…>` — the session is always
/// named explicitly and the inherited zellij env is scrubbed by [`run`], so
/// no invocation can be silently retargeted (or hang) via
/// `ZELLIJ_SESSION_NAME`.
fn action_argv(session: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "zellij".to_string(),
        "--session".to_string(),
        session.to_string(),
        "action".to_string(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    argv
}

/// Create a named tab running `command` in `cwd`.
///
/// The one place a [`TabLabel`] is spent (#20): the human-readable name exists
/// to be *displayed*, and taking the label by type here means nothing else can
/// accidentally be handed one.
///
/// Deliberately **without** `--close-on-exit`: per the #5 resolution the
/// EXITED tab is the post-mortem.
pub fn new_tab_argv(
    session: &str,
    label: &TabLabel,
    cwd: &Path,
    command: &[String],
) -> Vec<String> {
    let mut argv = action_argv(
        session,
        &[
            "new-tab",
            "--name",
            &label.to_string(),
            "--cwd",
            &cwd.to_string_lossy(),
        ],
    );
    argv.push("--".to_string());
    argv.extend(command.iter().cloned());
    argv
}

/// Focus an existing tab by name (the reason tabs, not panes, host tickets).
pub fn go_to_tab_argv(session: &str, tab: &str) -> Vec<String> {
    action_argv(session, &["go-to-tab-name", tab])
}

/// List a session's tab names.
pub fn query_tab_names_argv(session: &str) -> Vec<String> {
    action_argv(session, &["query-tab-names"])
}

/// Ask the client of `from` to switch to another session — the same gesture
/// as zellij's own session switcher, used when the ticket's project is not
/// the session `wf` is running in.
pub fn switch_session_argv(from: &str, to: &str) -> Vec<String> {
    action_argv(from, &["switch-session", to])
}

/// Attach a terminal to a session. Run as a **child** of `wf`, never
/// `exec`ed: detaching must return to the TUI.
pub fn attach_argv(session: &str) -> Vec<String> {
    vec![
        "zellij".to_string(),
        "attach".to_string(),
        session.to_string(),
    ]
}

/// Create a session with no client attached.
pub fn create_session_argv(session: &str) -> Vec<String> {
    vec![
        "zellij".to_string(),
        "attach".to_string(),
        "--create-background".to_string(),
        session.to_string(),
    ]
}

/// Drop a session's serialized state, so recreating the name cannot
/// resurrect a stale layout (#5 findings §5).
pub fn delete_session_argv(session: &str) -> Vec<String> {
    vec![
        "zellij".to_string(),
        "delete-session".to_string(),
        session.to_string(),
    ]
}

/// A session name as `zellij list-sessions` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Not known to zellij at all.
    Missing,
    /// Running (attachable, and `zellij action` reaches it).
    Live,
    /// Dead but serialized — attaching would resurrect it stale.
    Exited,
}

/// Classify one session name from `zellij list-sessions --no-formatting`
/// output. Lines look like
/// `name [Created 3h ago] (EXITED - attach to resurrect)`.
pub fn session_state(list_sessions_stdout: &str, session: &str) -> SessionState {
    for line in list_sessions_stdout.lines() {
        let line = line.trim();
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        if name != session {
            continue;
        }
        return if line.contains("(EXITED") {
            SessionState::Exited
        } else {
            SessionState::Live
        };
    }
    SessionState::Missing
}

/// Find this ticket's tab among a session's tab names, returning the name
/// zellij reported for it — which is what `go-to-tab-name` answers to, and is
/// *not* necessarily the label this ticket would generate now (the tab may have
/// been created before the issue was retitled).
///
/// Lookup is by **key**, never by label. Matching tolerates anything after the
/// key — the title in a label, zellij's activity markers, or both — but the key
/// must be a whole leading token, so `wayfinder#1` never matches `wayfinder#16`.
pub fn find_tab<'a>(names: &'a [String], key: &TabKey) -> Option<&'a str> {
    let key = key.to_string();
    names.iter().map(|n| n.trim()).find(|n| {
        **n == key
            || n.strip_prefix(key.as_str())
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

/// Does this ticket's tab already exist? [`find_tab`], for callers that only
/// need the answer.
///
/// Takes a [`TabKey`], which is the whole point of there being two types — a
/// label carries a title that changes under the tab, so it must not be
/// admissible here:
///
/// ```
/// # use wf::launch::{tab_exists, tab_key};
/// # use wf::model::{classify, Ticket, TicketType};
/// let ticket = Ticket {
///     repo: "blooop/wayfinder".to_string(),
///     number: 20,
///     title: "Readable agent tab names".to_string(),
///     status: classify(true, false, vec![]),
///     ticket_type: TicketType::Task,
/// };
/// let names = vec!["wayfinder#20 Readable agent".to_string()];
/// assert!(tab_exists(&names, &tab_key(&ticket)));
/// ```
///
/// The same call with the label does not compile:
///
/// ```compile_fail
/// # use wf::launch::{tab_exists, tab_label};
/// # use wf::model::{classify, Ticket, TicketType};
/// let ticket = Ticket {
///     repo: "blooop/wayfinder".to_string(),
///     number: 20,
///     title: "Readable agent tab names".to_string(),
///     status: classify(true, false, vec![]),
///     ticket_type: TicketType::Task,
/// };
/// let names = vec!["wayfinder#20 Readable agent".to_string()];
/// tab_exists(&names, &tab_label(&ticket));
/// ```
pub fn tab_exists(names: &[String], key: &TabKey) -> bool {
    find_tab(names, key).is_some()
}

/// Is this tab name one of ours, i.e. does it start with a `<repo>#<number>`
/// key? True of a bare key and of a full label alike.
pub fn is_agent_tab(name: &str) -> bool {
    TabKey::parse(name).is_some()
}

/// How many agent tabs these names contain — the AFK slot's count (#1's
/// reserved line, filled in per #7's "the tab is the supervision"). Counts by
/// leading key, so a labelled tab counts exactly like a bare-key one.
pub fn count_agent_tabs(names: &[String]) -> usize {
    names.iter().filter(|n| is_agent_tab(n)).count()
}

/// Whether `wf` itself must step aside after the tab exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Suspend the TUI, run this argv as a child, then restore and refresh.
    Suspend(Vec<String>),
    /// Keep the TUI up: the tab runs on its own (AFK), or the zellij client
    /// was moved to it by native navigation while `wf` keeps running in its
    /// own tab.
    Stay,
}

/// Decide the handoff. AFK never attaches; inside zellij, `wf` is not the
/// thing that owns the terminal, so it stays up and the *client* moves.
pub fn handoff(mode: Mode, host: &Host, session: &str) -> Handoff {
    match (mode, host) {
        (Mode::Afk, _) => Handoff::Stay,
        (Mode::Hitl, Host::Outside) => Handoff::Suspend(attach_argv(session)),
        (Mode::Hitl, Host::Inside(_) | Host::InsideUnnamed) => Handoff::Stay,
    }
}

/// The invocations that move the current zellij client onto the launched
/// tab, in order. Empty for AFK (no focus steal — #7) and outside zellij
/// (the attach in [`handoff`] does the moving).
///
/// Targets the [`OpenTab`] the seam actually opened, not a name regenerated
/// from the ticket: `go-to-tab-name` is an exact match and a silent no-op when
/// it misses (measured on zellij 0.44.3, #20), so a ticket retitled after its
/// tab was created would otherwise dedupe correctly and then fail to focus.
pub fn focus_steps(host: &Host, launch: &Launch, tab: &OpenTab) -> Vec<Vec<String>> {
    match (launch.mode, host) {
        (Mode::Afk, _) => Vec::new(),
        (Mode::Hitl, Host::Outside) => Vec::new(),
        (Mode::Hitl, Host::InsideUnnamed) => vec![go_to_tab_argv(&launch.session, &tab.name)],
        (Mode::Hitl, Host::Inside(current)) if *current == launch.session => {
            vec![go_to_tab_argv(&launch.session, &tab.name)]
        }
        (Mode::Hitl, Host::Inside(current)) => vec![
            // Point the target session at the tab first, then move this
            // client over — the session-switcher gesture, no new binding.
            go_to_tab_argv(&launch.session, &tab.name),
            switch_session_argv(current, &launch.session),
        ],
    }
}

/// Whether an AFK spawn could steal the current client's focus: only when
/// the tab is created in the very session `wf` is displayed in. The caller
/// then restores focus to the tab that had it.
pub fn afk_steals_focus(host: &Host, launch: &Launch) -> bool {
    launch.mode == Mode::Afk && matches!(host, Host::Inside(current) if *current == launch.session)
}

/// Parse the active tab's name out of `zellij action current-tab-info`
/// (`name: …` / `id: …` / `position: …`).
pub fn parse_current_tab_name(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("name:"))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Whether a tab had to be created, or was already there. Returned so
/// callers (and the integration test) can prove create-then-focus-by-name
/// is idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabOutcome {
    Created,
    Existed,
}

/// The tab a launch resolved to: whether it had to be created, **and** the name
/// zellij knows it by.
///
/// Both fields are always meaningful, so this is a product and not a sum: what
/// varies is only where the name came from — the label just written for a
/// `Created` tab, the tab's own (possibly older) label for an `Existed` one.
/// Producible only by [`create_or_focus_tab`], so a focus target can never be a
/// name nobody looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTab {
    outcome: TabOutcome,
    name: String,
}

impl OpenTab {
    /// Was the tab created just now, or already there?
    pub fn outcome(&self) -> TabOutcome {
        self.outcome
    }

    /// The name zellij will answer to for this tab.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Run a zellij invocation, scrubbing the inherited zellij variables so it
/// cannot be silently retargeted at — or hang on — the session those name.
/// The exit status is *not* trusted (`zellij action` returns 0 on failure);
/// callers verify by re-reading state.
async fn run(argv: &[String], cwd: Option<&Path>) -> Result<String> {
    let (program, args) = argv.split_first().context("empty zellij invocation")?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("ZELLIJ_PANE_ID");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .await
        .with_context(|| format!("running `{}`", argv.join(" ")))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn list_sessions() -> Result<String> {
    run(
        &[
            "zellij".to_string(),
            "list-sessions".to_string(),
            "--no-formatting".to_string(),
        ],
        None,
    )
    .await
}

/// A session's live tab names.
pub async fn query_tab_names(session: &str) -> Result<Vec<String>> {
    let stdout = run(&query_tab_names_argv(session), None).await?;
    Ok(stdout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Make sure `session` exists and is live, creating it rooted at `cwd` if
/// not. An EXITED session of the same name is deleted first so its stale
/// serialized layout cannot be resurrected.
pub async fn ensure_session(session: &str, cwd: &Path) -> Result<()> {
    match session_state(&list_sessions().await?, session) {
        SessionState::Live => return Ok(()),
        SessionState::Exited => {
            run(&delete_session_argv(session), None).await?;
        }
        SessionState::Missing => {}
    }
    run(&create_session_argv(session), Some(cwd)).await?;
    // Exit codes prove nothing here: verify by re-reading the session list.
    match session_state(&list_sessions().await?, session) {
        SessionState::Live => Ok(()),
        state => bail!("could not create zellij session `{session}` ({state:?})"),
    }
}

/// Create the ticket's tab if it is absent, leaving an existing one alone —
/// the create-**or-focus** half of the seam. `command` is what the tab
/// runs; the caller keeps focus decisions to itself.
///
/// The existence check is by [`TabKey`] (#20): a tab found under an older
/// label is still this ticket's tab, and its reported name comes back in the
/// [`OpenTab`] so the caller focuses the tab that is there rather than the name
/// it would write today.
pub async fn create_or_focus_tab(
    session: &str,
    label: &TabLabel,
    cwd: &Path,
    command: &[String],
) -> Result<OpenTab> {
    let key = label.key();
    if let Some(name) = find_tab(&query_tab_names(session).await?, key) {
        return Ok(OpenTab {
            outcome: TabOutcome::Existed,
            name: name.to_string(),
        });
    }
    run(&new_tab_argv(session, label, cwd, command), None).await?;
    // Again: verify by name, never by exit code.
    match find_tab(&query_tab_names(session).await?, key) {
        Some(name) => Ok(OpenTab {
            outcome: TabOutcome::Created,
            name: name.to_string(),
        }),
        None => bail!("zellij did not create tab `{label}` in session `{session}`"),
    }
}

/// Perform a launch: ensure the session, create-or-focus the ticket's tab,
/// move the zellij client if that is this host's handoff, and report what the
/// caller must still do (suspend-and-attach, or nothing).
pub async fn execute(launch: &Launch, host: &Host) -> Result<(OpenTab, Handoff)> {
    ensure_session(&launch.session, &launch.cwd).await?;

    // An AFK tab must not steal focus; remember where focus is so it can be
    // put back after the spawn.
    let restore_to = if afk_steals_focus(host, launch) {
        parse_current_tab_name(
            &run(&action_argv(&launch.session, &["current-tab-info"]), None).await?,
        )
    } else {
        None
    };

    let opened = create_or_focus_tab(
        &launch.session,
        &launch.label,
        &launch.cwd,
        &launch.agent_argv(),
    )
    .await?;

    for step in focus_steps(host, launch, &opened) {
        run(&step, None).await?;
    }
    if let Some(tab) = restore_to {
        run(&go_to_tab_argv(&launch.session, &tab), None).await?;
    }

    Ok((opened, handoff(launch.mode, host, &launch.session)))
}

/// Count the agent tabs across the projects' sessions — the AFK status line.
/// Only *live* sessions are queried (a missing one would merely print an error,
/// but there is no reason to ask).
///
/// Unchanged by the key/label split (#20): it counts tab *shapes* per session
/// rather than looking any ticket up, and [`is_agent_tab`] recognises a key at
/// the head of a name whatever follows it.
pub async fn agent_tab_count(sessions: &[String]) -> usize {
    let listing = match list_sessions().await {
        Ok(listing) => listing,
        Err(_) => return 0,
    };
    let mut unique: Vec<&String> = sessions.iter().collect();
    unique.sort();
    unique.dedup();
    let mut total = 0;
    for session in unique {
        if session_state(&listing, session) != SessionState::Live {
            continue;
        }
        if let Ok(names) = query_tab_names(session).await {
            total += count_agent_tabs(&names);
        }
    }
    total
}

/// Read every session's tab names, for reconciliation (#19) — the impure half
/// of [`crate::autostart::reconcile`], and the only zellij traffic auto-start
/// adds per poll.
///
/// A session zellij does not have is recorded as holding **no** tabs, which is a
/// fact rather than a failure: a launch into it would create it. A session whose
/// query *errored* is left out of the map entirely, so reconciliation can tell
/// "no tabs" from "don't know" and refuse to spawn on the latter. If the session
/// listing itself fails nothing is known, so the empty map is returned and that
/// poll reconciles nothing.
pub async fn tabs_by_session(sessions: &[String]) -> crate::autostart::TabsBySession {
    let mut tabs = crate::autostart::TabsBySession::new();
    let Ok(listing) = list_sessions().await else {
        return tabs;
    };
    let mut unique: Vec<&String> = sessions.iter().collect();
    unique.sort();
    unique.dedup();
    for session in unique {
        if session_state(&listing, session) != SessionState::Live {
            tabs.insert(session.clone(), Vec::new());
            continue;
        }
        if let Ok(names) = query_tab_names(session).await {
            tabs.insert(session.clone(), names);
        }
    }
    tabs
}

/// Distinct session names across cached checkouts, for [`agent_tab_count`].
pub fn sessions_of(checkouts: &[Checkout]) -> Vec<String> {
    let mut sessions: Vec<String> = checkouts.iter().map(|c| c.session.clone()).collect();
    sessions.sort();
    sessions.dedup();
    sessions
}

/// Map issue numbers by repo slug — `/wayfinder`'s first argument.
pub type MapIssues = BTreeMap<String, u64>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Status, TicketType};

    fn titled(repo: &str, number: u64, title: &str) -> Ticket {
        Ticket {
            repo: repo.to_string(),
            number,
            title: title.to_string(),
            status: classify(true, false, vec![]),
            ticket_type: TicketType::Task,
        }
    }

    fn ticket(repo: &str, number: u64) -> Ticket {
        titled(repo, number, "the ticket")
    }

    fn checkout(path: &str, repo: &str, session: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(path),
            repo: repo.to_string(),
            session: session.to_string(),
        }
    }

    fn cache() -> Vec<Checkout> {
        vec![
            checkout("/data/k1/kinisi_ros", "kinisi/kinisi_ros", "k1"),
            checkout("/data/k2/kinisi_ros", "kinisi/kinisi_ros", "k2"),
            checkout("/data/proj/wayfinder", "blooop/wayfinder", "wayfinder"),
            checkout("/data/proj/dotfiles", "upstream/dotfiles", "dotfiles"),
        ]
    }

    #[test]
    fn the_key_is_short_repo_hash_number_and_carries_no_title() {
        assert_eq!(
            tab_key(&ticket("blooop/wayfinder", 16)).to_string(),
            "wayfinder#16"
        );
        // The short name, not the slug — but identity stays the full slug.
        assert_eq!(
            tab_key(&ticket("upstream/dotfiles", 5)).to_string(),
            "dotfiles#5"
        );
    }

    #[test]
    fn the_key_survives_a_retitle_but_the_label_does_not() {
        let before = titled("blooop/wayfinder", 20, "Tab names are unreadable");
        let after = titled("blooop/wayfinder", 20, "Readable agent tab names");
        // The defect this ticket exists to prevent: identity must not move
        // when the title does, or a retitled issue misses its own tab.
        assert_eq!(tab_key(&before), tab_key(&after));
        assert_eq!(tab_key(&after).to_string(), "wayfinder#20");
        assert_ne!(tab_label(&before), tab_label(&after));
        // …and a label always still answers with its own key.
        assert_eq!(tab_label(&before).key(), &tab_key(&after));
        // Even an emptied title leaves the key intact and the label usable.
        let untitled = titled("blooop/wayfinder", 20, "   ");
        assert_eq!(tab_key(&untitled), tab_key(&after));
        assert_eq!(tab_label(&untitled).short_title(), None);
        assert_eq!(tab_label(&untitled).to_string(), "wayfinder#20");
    }

    #[test]
    fn the_label_caps_the_title_on_a_word_boundary() {
        let label = |title: &str| tab_label(&titled("blooop/wayfinder", 20, title)).to_string();
        // Short enough: verbatim, no ellipsis.
        assert_eq!(label("Prove the seam"), "wayfinder#20 Prove the seam");
        // Exactly at the cap (18 chars) is not truncated.
        assert_eq!(
            label("123456789012345678"),
            "wayfinder#20 123456789012345678"
        );
        // Over the cap: cut at the last word that fits, marked with `…`.
        assert_eq!(
            label("Readable agent tab names: title in the label"),
            "wayfinder#20 Readable agent tab…"
        );
        assert_eq!(
            label("Auto-start AFK frontier tickets"),
            "wayfinder#20 Auto-start AFK…"
        );
        // Whitespace is collapsed — a tab name is one line.
        assert_eq!(
            label("  many\n\tspaces  here "),
            "wayfinder#20 many spaces here"
        );
        // No word boundary to break on: break inside the word rather than
        // dropping the title entirely.
        assert_eq!(
            label("Unsplittableverylongword tail"),
            "wayfinder#20 Unsplittableverylo…"
        );
        // Every capped title fits the budget (cap + the one ellipsis char).
        for title in [
            "Readable agent tab names: title in the label",
            "Unsplittableverylongword tail",
            "a b c d e f g h i j k l m n o p q r s t",
        ] {
            let short = tab_label(&titled("blooop/wayfinder", 20, title))
                .short_title()
                .expect("a title")
                .chars()
                .count();
            assert!(short <= TITLE_CAP + 1, "{title:?} capped to {short} chars");
        }
    }

    #[test]
    fn candidate_checkouts_filter_on_the_full_slug() {
        let cache = cache();
        let kinisi = candidate_checkouts(&cache, "kinisi/kinisi_ros");
        assert_eq!(
            kinisi
                .iter()
                .map(|c| c.session.as_str())
                .collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
        // A fork and its upstream share a short name: matching must not mix
        // them, so "blooop/dotfiles" has no candidate here.
        assert!(candidate_checkouts(&cache, "blooop/dotfiles").is_empty());
        assert_eq!(candidate_checkouts(&cache, "upstream/dotfiles").len(), 1);
    }

    #[test]
    fn one_candidate_never_prompts_and_none_is_unregistered() {
        let cache = cache();
        match plan(&cache, &ticket("blooop/wayfinder", 16), 1, Mode::Hitl) {
            Targets::One(launch) => {
                assert_eq!(launch.session, "wayfinder");
                assert_eq!(launch.cwd, PathBuf::from("/data/proj/wayfinder"));
                assert_eq!(launch.key().to_string(), "wayfinder#16");
                assert_eq!(launch.label.to_string(), "wayfinder#16 the ticket");
                assert_eq!(launch.map_issue, 1);
                assert_eq!(launch.ticket, 16);
            }
            other => panic!("expected One, got {other:?}"),
        }
        assert_eq!(
            plan(&cache, &ticket("blooop/dotfiles", 3), 2, Mode::Hitl),
            Targets::Unregistered
        );
        assert_eq!(
            plan(&[], &ticket("blooop/wayfinder", 16), 1, Mode::Afk),
            Targets::Unregistered
        );
    }

    #[test]
    fn several_checkouts_of_one_repo_offer_a_choice_of_sessions() {
        let launches = match plan(&cache(), &ticket("kinisi/kinisi_ros", 42), 7, Mode::Hitl) {
            Targets::Many(launches) => launches,
            other => panic!("expected Many, got {other:?}"),
        };
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches
                .iter()
                .map(|l| l.session.as_str())
                .collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
        // Same ticket, same tab identity, different hosting session/cwd.
        assert!(launches
            .iter()
            .all(|l| l.key().to_string() == "kinisi_ros#42"));
        assert_eq!(launches[1].cwd, PathBuf::from("/data/k2/kinisi_ros"));
    }

    #[test]
    fn hitl_runs_interactive_claude_with_one_prompt_argument() {
        let launch = match plan(&cache(), &ticket("blooop/wayfinder", 16), 1, Mode::Hitl) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launch.agent_argv(),
            vec!["claude".to_string(), "/wayfinder 1 16".to_string()]
        );
    }

    #[test]
    fn afk_runs_headless_claude_p_with_the_same_prompt() {
        let launch = match plan(&cache(), &ticket("blooop/wayfinder", 16), 1, Mode::Afk) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            launch.agent_argv(),
            vec![
                "claude".to_string(),
                "-p".to_string(),
                "/wayfinder 1 16".to_string()
            ]
        );
    }

    #[test]
    fn new_tab_names_the_session_carries_cwd_and_keeps_the_corpse() {
        let ticket = titled("blooop/wayfinder", 16, "Build 4 — launch seam");
        let launch = match plan(&cache(), &ticket, 1, Mode::Hitl) {
            Targets::One(l) => l,
            other => panic!("{other:?}"),
        };
        // The label — the *only* place it is spent — carries the title.
        assert_eq!(
            launch.new_tab_argv(),
            vec![
                "zellij",
                "--session",
                "wayfinder",
                "action",
                "new-tab",
                "--name",
                "wayfinder#16 Build 4 — launch…",
                "--cwd",
                "/data/proj/wayfinder",
                "--",
                "claude",
                "/wayfinder 1 16",
            ]
        );
        // #5: no --close-on-exit — the EXITED tab is the post-mortem.
        assert!(!launch.new_tab_argv().iter().any(|a| a == "--close-on-exit"));
    }

    #[test]
    fn focus_and_attach_invocations_are_named_and_session_targeted() {
        assert_eq!(
            go_to_tab_argv("k1", "kinisi_ros#42"),
            vec![
                "zellij",
                "--session",
                "k1",
                "action",
                "go-to-tab-name",
                "kinisi_ros#42"
            ]
        );
        assert_eq!(attach_argv("k1"), vec!["zellij", "attach", "k1"]);
        assert_eq!(
            create_session_argv("k1"),
            vec!["zellij", "attach", "--create-background", "k1"]
        );
        assert_eq!(
            delete_session_argv("k1"),
            vec!["zellij", "delete-session", "k1"]
        );
        assert_eq!(
            switch_session_argv("wayfinder", "k1"),
            vec![
                "zellij",
                "--session",
                "wayfinder",
                "action",
                "switch-session",
                "k1"
            ]
        );
    }

    #[test]
    fn host_comes_from_wfs_own_env_not_from_zellij_output() {
        assert_eq!(host_from_env(None, None), Host::Outside);
        // A stale session name with no $ZELLIJ is still "outside" — that is
        // exactly the variable that hangs zellij, so it is never trusted.
        assert_eq!(host_from_env(None, Some("dead-session")), Host::Outside);
        assert_eq!(
            host_from_env(Some("0"), Some("wayfinder")),
            Host::Inside("wayfinder".to_string())
        );
        assert_eq!(host_from_env(Some("0"), None), Host::InsideUnnamed);
        assert_eq!(host_from_env(Some("0"), Some("  ")), Host::InsideUnnamed);
    }

    fn launch_for(mode: Mode, session: &str) -> Launch {
        Launch {
            mode,
            session: session.to_string(),
            cwd: PathBuf::from("/data/proj/wayfinder"),
            label: tab_label(&titled("blooop/wayfinder", 16, "launch seam")),
            map_issue: 1,
            ticket: 16,
        }
    }

    /// The tab as the seam reported it — what focus must target.
    fn opened(outcome: TabOutcome, name: &str) -> OpenTab {
        OpenTab {
            outcome,
            name: name.to_string(),
        }
    }

    #[test]
    fn outside_zellij_hitl_suspends_and_attaches_as_a_child() {
        let launch = launch_for(Mode::Hitl, "wayfinder");
        assert_eq!(
            handoff(Mode::Hitl, &Host::Outside, "wayfinder"),
            Handoff::Suspend(vec![
                "zellij".to_string(),
                "attach".to_string(),
                "wayfinder".to_string()
            ])
        );
        // Nothing else moves the client: the attach does it.
        let tab = opened(TabOutcome::Created, "wayfinder#16 launch seam");
        assert!(focus_steps(&Host::Outside, &launch, &tab).is_empty());
    }

    #[test]
    fn inside_the_same_session_hitl_just_focuses_the_tab() {
        let launch = launch_for(Mode::Hitl, "wayfinder");
        let host = Host::Inside("wayfinder".to_string());
        let tab = opened(TabOutcome::Created, "wayfinder#16 launch seam");
        assert_eq!(handoff(Mode::Hitl, &host, "wayfinder"), Handoff::Stay);
        assert_eq!(
            focus_steps(&host, &launch, &tab),
            vec![go_to_tab_argv("wayfinder", "wayfinder#16 launch seam")]
        );
    }

    #[test]
    fn focus_targets_the_tab_that_is_there_not_the_label_regenerated_now() {
        // The tab was created before the issue was retitled, so it still wears
        // the old label. `go-to-tab-name` is an exact match and a silent no-op
        // when it misses (measured, zellij 0.44.3), so focusing the *current*
        // label would leave the client where it was.
        let launch = launch_for(Mode::Hitl, "wayfinder");
        let host = Host::Inside("wayfinder".to_string());
        let stale = opened(TabOutcome::Existed, "wayfinder#16 prove the seam");
        assert_eq!(
            focus_steps(&host, &launch, &stale),
            vec![go_to_tab_argv("wayfinder", "wayfinder#16 prove the seam")]
        );
        assert_ne!(stale.name(), launch.label.to_string());
    }

    #[test]
    fn inside_another_session_hitl_switches_sessions_natively() {
        let launch = launch_for(Mode::Hitl, "k1");
        let host = Host::Inside("wayfinder".to_string());
        let tab = opened(TabOutcome::Created, "wayfinder#16 launch seam");
        assert_eq!(handoff(Mode::Hitl, &host, "k1"), Handoff::Stay);
        assert_eq!(
            focus_steps(&host, &launch, &tab),
            vec![
                go_to_tab_argv("k1", "wayfinder#16 launch seam"),
                switch_session_argv("wayfinder", "k1")
            ]
        );
    }

    #[test]
    fn afk_never_attaches_and_never_steals_focus() {
        for host in [
            Host::Outside,
            Host::InsideUnnamed,
            Host::Inside("wayfinder".to_string()),
            Host::Inside("other".to_string()),
        ] {
            let launch = launch_for(Mode::Afk, "wayfinder");
            let tab = opened(TabOutcome::Created, "wayfinder#16 launch seam");
            assert_eq!(handoff(Mode::Afk, &host, "wayfinder"), Handoff::Stay);
            assert!(
                focus_steps(&host, &launch, &tab).is_empty(),
                "host {host:?}"
            );
        }
        // Only a spawn into wf's own session can steal focus, so only that
        // case restores it.
        assert!(afk_steals_focus(
            &Host::Inside("wayfinder".to_string()),
            &launch_for(Mode::Afk, "wayfinder")
        ));
        assert!(!afk_steals_focus(
            &Host::Inside("other".to_string()),
            &launch_for(Mode::Afk, "wayfinder")
        ));
        assert!(!afk_steals_focus(
            &Host::Outside,
            &launch_for(Mode::Afk, "wayfinder")
        ));
        assert!(!afk_steals_focus(
            &Host::Inside("wayfinder".to_string()),
            &launch_for(Mode::Hitl, "wayfinder")
        ));
    }

    #[test]
    fn session_state_reads_the_listing_not_an_exit_code() {
        let listing = "\
remarkable-newt [Created 12days ago] (EXITED - attach to resurrect)
k1 [Created 4days ago] (EXITED - attach to resurrect)
wayfinder [Created 3h 1m 23s ago] (current)
kinisi [Created 1h ago] \n";
        assert_eq!(session_state(listing, "wayfinder"), SessionState::Live);
        assert_eq!(session_state(listing, "kinisi"), SessionState::Live);
        assert_eq!(session_state(listing, "k1"), SessionState::Exited);
        assert_eq!(session_state(listing, "k2"), SessionState::Missing);
        assert_eq!(session_state("", "k1"), SessionState::Missing);
        // A prefix of a session name is not that session.
        assert_eq!(session_state(listing, "way"), SessionState::Missing);
    }

    fn tab_names(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tab_lookup_is_by_key_and_tolerates_a_title_and_a_decoration_suffix() {
        let names = tab_names(&["Tab #1", "wayfinder#16", "kinisi_ros#42 ⏳"]);
        let key = |repo: &str, n: u64| tab_key(&ticket(repo, n));
        assert!(tab_exists(&names, &key("blooop/wayfinder", 16)));
        assert!(tab_exists(&names, &key("kinisi/kinisi_ros", 42)));
        assert!(
            !tab_exists(&names, &key("blooop/wayfinder", 1)),
            "#1 must not match #16"
        );
        assert!(!tab_exists(&names, &key("blooop/wayfinder", 166)));
        assert!(!tab_exists(&names, &key("upstream/dotfiles", 5)));

        // The point of #20: a labelled tab — with zellij's activity marker on
        // top of the title — is still found by the ticket's key, and found by
        // the key the ticket generates *now*, not the one in the name.
        let labelled = tab_names(&[
            "Tab #1",
            "wayfinder#16 Build 4 — launch… ⏳",
            "kinisi_ros#42 old title",
        ]);
        let retitled = titled("blooop/wayfinder", 16, "something else entirely");
        assert!(tab_exists(&labelled, &tab_key(&retitled)));
        assert_eq!(
            find_tab(&labelled, &tab_key(&retitled)),
            Some("wayfinder#16 Build 4 — launch… ⏳"),
            "the found name is what go-to-tab-name must be given"
        );
        // Still no partial-number matching once titles are in play.
        assert!(!tab_exists(&labelled, &key("blooop/wayfinder", 1)));
        assert_eq!(find_tab(&labelled, &key("blooop/wayfinder", 7)), None);
    }

    #[test]
    fn agent_tabs_are_counted_by_the_repo_hash_number_shape() {
        let names = tab_names(&[
            "Tab #1",
            "wayfinder#16",
            "kinisi_ros#42 ⏳",
            "wayfinder#",
            "#5",
            "notes",
        ]);
        assert_eq!(count_agent_tabs(&names), 2);
        assert!(is_agent_tab("wayfinder#16"));
        assert!(
            !is_agent_tab("Tab #1"),
            "zellij's default tab names are not ours"
        );
        assert!(!is_agent_tab("wayfinder#+16"), "digits only");
    }

    #[test]
    fn counting_needs_no_change_for_labels_titles_ride_behind_the_key() {
        // Why `agent_tab_count` is untouched by #20: it counts shapes per
        // session, and the shape test reads the leading key whatever follows.
        let labelled = tab_names(&[
            "Tab #1",
            "wayfinder#16 Build 4 — launch…",
            "kinisi_ros#42 Auto-start AFK… ⏳",
            "wayfinder#20 Readable agent tab…",
            "notes on the release",
        ]);
        assert_eq!(count_agent_tabs(&labelled), 3);
        assert_eq!(
            TabKey::parse("wayfinder#20 Readable agent tab… ⏳"),
            Some(tab_key(&ticket("blooop/wayfinder", 20))),
            "parsing a displayed name back gives the ticket's key"
        );
        assert_eq!(TabKey::parse("notes on the release"), None);
        assert_eq!(TabKey::parse(""), None);
    }

    #[test]
    fn current_tab_name_parses_out_of_current_tab_info() {
        let stdout = "name: Tab #3 ⏳\nid: 2\nposition: 1\n";
        assert_eq!(parse_current_tab_name(stdout).as_deref(), Some("Tab #3 ⏳"));
        assert_eq!(parse_current_tab_name("no active tab found"), None);
        assert_eq!(parse_current_tab_name("name:   \nid: 1"), None);
    }

    #[test]
    fn sessions_of_dedups_shared_sessions() {
        assert_eq!(
            sessions_of(&cache()),
            vec![
                "dotfiles".to_string(),
                "k1".to_string(),
                "k2".to_string(),
                "wayfinder".to_string()
            ]
        );
    }

    #[test]
    fn describe_names_the_tab_by_key_and_its_session() {
        // The notice line stays short: the key, not the label.
        assert_eq!(launch_for(Mode::Afk, "k1").describe(), "wayfinder#16 in k1");
        // Status is irrelevant to launching: a done ticket still gets a tab.
        let done = Ticket {
            repo: "blooop/wayfinder".to_string(),
            number: 2,
            title: "done".to_string(),
            status: Status::Done,
            ticket_type: TicketType::Task,
        };
        assert_eq!(tab_key(&done).to_string(), "wayfinder#2");
        assert_eq!(tab_label(&done).to_string(), "wayfinder#2 done");
    }
}
