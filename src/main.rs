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
//!    ([`wf::launch::Launch::exec`]).
//!
//! This file is argv and the commands that answer on a stream — `--version`,
//! `wf skills`, `wf reap`. The picker itself is [`picker`], a module of its
//! own, because `wf reap` deletes workspaces for a living and the picker must
//! be provably unable to: that is one grep over one file only if the two do not
//! share one (#137).

use anyhow::{bail, Context, Result};
use tokio::signal::unix::{signal, SignalKind};

use wf::launch::Agent;
use wf::reap;
use wf::skills;

mod picker;

/// Test scaffolding shared with the library's own tests — the same
/// `src/probe.rs`, compiled into this crate too, because that is the only way
/// the binary's tests can reach it. Never compiled into a release.
#[cfg(test)]
mod probe;

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
            Ok(())
        }
        Invocation::Reject(text) => {
            eprintln!("{text}");
            std::process::exit(2)
        }
        // Both skills paths answer on a stream and exit, like `--version`:
        // no terminal, no `gh`, so they stay usable in a package test and in
        // whatever script installs the tool.
        Invocation::SkillsReport => run_skills(false),
        Invocation::SkillsInstall => run_skills(true),
        Invocation::Reap { yes, insist } => run_reap(yes, insist).await,
        // And the one shape that opens the TUI, which is the whole of what this
        // arm may do. Everything the picker touches lives in a module that
        // cannot name a deletion — see [`picker`] — and this line is what keeps
        // `main.rs`, where `wf reap` does its deleting, out of that path
        // entirely. Pinned by `the_tui_invocation_is_nothing_but_the_picker`.
        Invocation::Tui => picker::run_picker().await,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().copied().map(String::from).collect()
    }

    #[test]
    fn the_tui_invocation_is_nothing_but_the_picker() {
        // The composition site, structurally. `wf reap` lives in this file and
        // deletes workspaces for a living, so this file can never carry the
        // denylist [`picker`] does — and the picker path through it is exactly
        // one expression long, which is a thing a grep *can* pin. Anything else
        // wired into this arm, before or after or instead, fails here; anything
        // wired inside the picker fails
        // `picker::tests::no_deletion_is_reachable_from_the_picker`.
        let code = probe::code_only(include_str!("main.rs"));
        assert!(
            code.contains("Invocation::Tui => picker::run_picker().await,"),
            "opening the TUI must be the whole of what that arm does"
        );
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
