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
//! This file is argv and nothing else: it decides what was asked for and hands
//! it to whoever answers. `wf skills` is [`run_skills`] here because linking a
//! directory is a few lines; `wf reap` is [`wf::reap::run`] in the *library*,
//! because reaping deletes workspaces and the deletion it calls
//! ([`wf::reap`]'s private `remove`) is not something this crate may name. The
//! picker is [`picker`], a module of its own.
//!
//! Half of that arrangement is a fact about visibility, and needs no checking:
//! the deletion is private to a module in another crate, so no alias, helper or
//! submodule of this binary can call it, and the edit that tries does not
//! compile.
//!
//! The other half is not. `wf::reap::run` has to be public for the arm below to
//! dispatch it, and it *contains* the deletion — `reap::run(true, true)` is a
//! forced reap with the unsaved-work guard waived. Nothing in the language
//! stops a line anywhere in this crate calling it, so what stands in its place
//! is a grep: [`tests::the_binary_writes_reap_only_where_it_is_listed_here`]
//! holds the whole list of lines in this file that write the word `reap`, and
//! `picker.rs` may not write it at all.
//!
//! That is a guard over the source text of two files, and it is worth what that
//! is: it catches a cleanup wired in here by accident, which is the mistake
//! anyone is actually likely to make. It does not stop someone routing around
//! it. A third file calling `wf::reap::run` is caught by neither grep, and
//! neither is a second name for the module re-exported from the library. See
//! [`picker`] for the guard that watches a run rather than a file, and #137 for
//! the crate split that would settle it.

use anyhow::Result;
use tokio::signal::unix::{signal, SignalKind};

use wf::emit;
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
        // The one place in this binary that reaches a deletion, and it reaches
        // it as a whole command: prompt, plan, and the private `remove` this
        // crate could not spell if it wanted to. Listed, with every other line
        // here that writes the word, by
        // `the_binary_writes_reap_only_where_it_is_listed_here` — an arm that
        // merely exists says nothing about the rest of the file, and a prologue
        // before this match is how an earlier round deleted a workspace on
        // `wf --version`.
        Invocation::Reap { yes, insist } => reap::run(yes, insist).await,
        // And the one shape that opens the TUI, which is the whole of what this
        // arm may do — the picker is handed the process, not a deletion, and
        // this line is what keeps the rest of this file out of its path.
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

    /// Every line of `code` that writes `token` as a word of its own — not as
    /// part of a longer identifier, so `parse_reap` and `Reap` are not
    /// occurrences of `reap`.
    ///
    /// A *word*, and that is the whole technique. Counting `"reap::"` was green
    /// for `use wf::reap as tidy;`, and counting `"wf::reap"` was green for
    /// `use crate::reap as tidy;` — each round found another spelling of the
    /// same path. There is no spelling of the module that does not write its
    /// name, so the name is what is counted. `picker.rs` has been immune since
    /// round 5 for exactly this reason; this is that technique, applied here.
    fn lines_naming<'a>(code: &'a str, token: &str) -> Vec<&'a str> {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        code.lines()
            .filter(|line| {
                line.match_indices(token).any(|(at, _)| {
                    !line[..at].chars().next_back().is_some_and(ident)
                        && !line[at + token.len()..].chars().next().is_some_and(ident)
                })
            })
            .map(str::trim)
            .collect()
    }

    /// `code` with the `USAGE` literal cut out — from its `const` line to the
    /// line that closes the string.
    ///
    /// `USAGE` is help text. It writes `wf reap` because that is the command it
    /// documents, and it calls nothing. Leaving it in [`lines_naming`]'s input
    /// put two of its hand-wrapped lines into the list below, which turned that
    /// list into a pin on the wrapping: rewording the `wf reap` paragraph — no
    /// capability changed, the same code, the same seven sites — failed the
    /// guard, and failed it with a message saying the file wrote `reap`
    /// somewhere new, which was not true. Cutting the literal out leaves the
    /// list five lines of code, which is what the guard is about.
    ///
    /// # Panics
    ///
    /// If `USAGE` is not found, or its literal is not closed by a line ending
    /// `";` — the same fail-closed posture as [`probe::code_only`]: a shape
    /// this cannot locate is refused rather than skipped.
    fn without_usage(code: &str) -> String {
        let lines: Vec<&str> = code.lines().collect();
        let opens = lines
            .iter()
            .position(|line| line.starts_with("const USAGE: &str = \""))
            .expect("main.rs defines USAGE at column 0");
        let closes = opens
            + lines[opens..]
                .iter()
                .position(|line| line.trim_end().ends_with("\";"))
                .expect("the USAGE literal is closed by a line ending `\";`");
        lines
            .iter()
            .enumerate()
            .filter(|(at, _)| !(opens..=closes).contains(at))
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_binary_writes_reap_only_where_it_is_listed_here() {
        // What is left for a grep to say once the deletion itself is out of
        // reach.
        //
        // `reap::remove` is private to the library's `reap` module, so nothing
        // in this crate can call it — that half needs no test, it is a compile
        // error. What this crate *can* still name is `reap::run`, the whole
        // `wf reap` command, prompt and plan and all, and `reap::run(true,
        // true)` is a forced deletion. A previous round's escape was exactly
        // that as a prologue before the match, and `wf --version` reaped a
        // workspace and exited 0.
        //
        // So this is the whole list of lines of *code* in this file that write
        // the word `reap`, compared as a list rather than as a count. All five
        // are code: the import, the two parse arms — one of which calls
        // `parse_reap` — the rejection message, and the dispatch arm that calls
        // `reap::run`. The help text is cut out first (see `without_usage`),
        // because it is prose and pinning its line wrapping here made the guard
        // fail on rewraps and say something untrue when it did.
        //
        // Any new line that writes the word fails here, whatever it writes
        // around it: an alias (`use wf::reap as tidy;`, `use crate::reap as
        // tidy;`), a path through the crate root, a call split over two lines.
        // The module cannot be reached without its name being written
        // somewhere, and every somewhere is below.
        //
        // What this does **not** reach: a third file calling `wf::reap::run`,
        // and a re-export that gives the module a second name in the library.
        // It is a grep over one file, and a grep over one file cannot see
        // either. What it is good for is the accident — a maintainer wiring a
        // cleanup in without noticing — and that is the claim the module doc
        // makes for it too.
        let code = probe::code_only("main.rs", include_str!("main.rs"));
        assert_eq!(
            lines_naming(&without_usage(&code), "reap"),
            [
                "use wf::reap;",
                "\"reap\" => Invocation::Reap {", // }
                "[first, rest @ ..] if first == \"reap\" => parse_reap(rest),",
                "\"wf: unknown reap argument {other:?} (expected `-y` or `-f`)\\n{USAGE}\"",
                "Invocation::Reap { yes, insist } => reap::run(yes, insist).await,",
            ],
            "the lines of code in this file that write `reap` are no longer the five \
             listed here. A line that is not in the list is a way this binary reaches \
             `wf::reap::run`, which is a forced deletion when it is called with both \
             flags; a listed line that is gone means the list is stale. Read the diff \
             and decide which before editing the list"
        );
        // The dangling opening brace in the second entry is real: the arm it
        // quotes opens a struct literal and rustfmt wraps the line there. The
        // trailing comment that closes it is not decoration —
        // `probe::code_only` counts braces as text to find where this module
        // ends, so a string that opens one and never closes it fails this
        // file's own guard. That is the fail-closed cost described in that
        // function's doc, paid here, in the one place this file pays it.

        // The other half of the same claim, at the other arm: opening the TUI
        // is the whole of what `wf` with no arguments does.
        assert!(
            code.contains("Invocation::Tui => picker::run_picker().await,"),
            "opening the TUI must be the whole of what that arm does"
        );

        // And the deletion that does not go through `reap` at all. `main` is
        // argv and dispatch; it starts no subprocess of its own. A prologue
        // reading `Command::new("dl").args([id, "rm", "--force"]).output()` is
        // the most ordinary shape an accidental cleanup takes, it runs on every
        // invocation including `wf --version`, and until this line nothing in
        // this file looked for it. `process::` and `tokio::spawn` are *not*
        // added beside it: this file genuinely uses both, for the signal
        // handler and the exit code.
        assert!(
            !code.contains("Command"),
            "this file dispatches; it does not run anything itself, and a \
             subprocess started here runs on every `wf` invocation"
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
