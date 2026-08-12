//! What the installed `devlaunch` actually says, held against what `wf`
//! believes it says.
//!
//! Every other test in this repository that touches `dl` hands the code a
//! string this repository wrote. `src/probe.rs` shims it; so, deliberately,
//! does `tests/live_launch_exec.rs`, because a machine with devlaunch installed
//! and a machine without one must not take different paths through the same
//! test. That is the right call for those tests and it leaves exactly one thing
//! unchecked: whether the fixtures are still a description of the program.
//!
//! They were not, twice. `dl <workspace> up` shipped in `wf` 0.14.0 before
//! devlaunch 0.0.24 carried it. `unsaved` changed from a string to a one-key
//! object in the same release, and this repo learned about it afterwards. Both
//! are the same failure: two repositories agreeing in prose, with nothing
//! executed that could disagree.
//!
//! This file is the executed part. It runs a **real** `dl` — the one `pixi.toml`
//! installed, at a version that file chose — and asks it the questions `wf`
//! asks it.
//!
//! ```text
//! pixi run -e default contract  # no devlaunch at all
//! pixi run -e floor   contract  # devlaunch == launch::DEVLAUNCH_FLOOR
//! pixi run -e latest  contract  # whatever pixi.lock resolved
//! pixi run -e stale   contract  # 0.0.23, below the floor
//! ```
//!
//! # Reading a weaker `dl`, and then behaving
//!
//! Two different claims, and both are made here. Most of the tests are about
//! what `wf` *reads* — the version, the listing, the `unsaved` field. Three are
//! about what it then *does*, which is the half that reaches a user:
//! [`a_devcontainer_repo_is_isolated_only_when_a_real_dl_can_carry_it`] asks
//! `Isolation::detect` in a checkout that really carries a devcontainer (this
//! one) and requires a host launch when `dl` is absent or below the floor,
//! [`a_launch_that_fell_back_says_why_and_names_the_version`] requires the
//! notice that says so, and [`a_reap_with_no_dl_refuses_instead_of_guessing`]
//! runs the real binary and requires it to fail rather than mistake "cannot
//! see the workspaces" for "there are none".
//!
//! # Everything here fails closed
//!
//! It is not runnable outside those environments and does not try to be: with
//! no `WF_CONTRACT_*` in the environment **every** test here panics saying so,
//! rather than quietly testing whichever `dl` the developer happens to have.
//! That distinction is the whole point — a contract test that silently accepts
//! the ambient install is the shim again, wearing a different hat.
//!
//! The same rule governs the variables' *values*, and it is not decoration. An
//! earlier draft compared `WF_CONTRACT_UNSAVED` for equality and returned early
//! otherwise; typing `objekt` into `pixi.toml` then disabled both of the tests
//! that read it and the file still reported seven passes. Anything unrecognised
//! is a panic now, and [`Contract::read`] is the one place that decides.
//!
//! # The calls that change the machine
//!
//! `dl <id> rm` and `dl <ws> up` build and destroy containers, so they cannot
//! simply be run. They are not left unchecked either: `dl`'s only devpod spawn
//! is `subprocess.run(["devpod", ...])` — a bare name on PATH — so a recording
//! `devpod` in front of the **real** `dl` runs devlaunch's own argument
//! parsing, its own workspace resolution and its own decision about what to ask
//! devpod, and stops where the container would begin.
//!
//! That is the inverse of the shim this file exists to escape. `src/probe.rs`
//! shims `dl` itself, which is how its fixtures were able to drift; here the
//! real `dl` is the subject and the shim stands one layer below it, in for the
//! daemon rather than for the program under test. The argvs come from
//! [`wf::reap::removal_argv`], [`wf::launch::prewarm_argv`] and
//! [`wf::launch::isolated_argv`] rather than being typed out here — an argv a
//! contract test spells out for itself only proves the test agrees with the
//! test.
//!
//! It is worth what it costs: [`the_prewarm_wf_sends_is_the_verb_the_floor_exists_for`]
//! sends `wf`'s own prewarm argv to a real devlaunch 0.0.23 and watches it come
//! back `Unknown command 'up'`, which is the 0.14.0 regression itself, running.
//!
//! Everything else here is read-only, and all of it runs under a scratch `HOME`
//! so it cannot see or touch a real workspace. The scratch home matters for a
//! second reason: a listing read from the developer's own machine would assert
//! against whatever they happen to have cloned.

use std::path::{Path, PathBuf};
use std::process::Command;

use wf::launch::{Agent, Devlaunch, Isolation, DEVLAUNCH_FLOOR, UNSAVED_IS_AN_OBJECT};
use wf::reap::{parse_workspaces, Unsaved};

/// How `pixi.toml` tells this file what environment it is in.
///
/// Four variables rather than one name, because three of them are *facts about
/// the release* and a single environment name would make this file re-derive
/// them — which is the transcription problem again, one repository further in.
/// [`Role`] is the exception and carries no facts: it decides which assertions
/// apply, not what any of them expect.
const ROLE: &str = "WF_CONTRACT_ROLE";
const EXPECT: &str = "WF_CONTRACT_EXPECT";
const VERSION: &str = "WF_CONTRACT_DL";
const UNSAVED: &str = "WF_CONTRACT_UNSAVED";

/// Which of the four environments this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// No devlaunch at all, and `scripts/without-dl.sh` has taken away any the
    /// developer had. The environment `wf` has to keep working in.
    None,
    /// Pinned to exactly [`DEVLAUNCH_FLOOR`].
    Floor,
    /// Whatever `pixi.lock` resolved — no version is written down for it.
    Latest,
    /// Pinned below the floor.
    Stale,
}

/// What `wf` must make of this release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Usable,
    TooOld,
}

/// How this release writes `unsaved` in `dl --ls --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// devlaunch ≥ 0.0.24: a one-key object.
    Object,
    /// devlaunch ≤ 0.0.23: a bare sentence, or `null`.
    Sentence,
}

/// The environment's claims about the `dl` it installed, all of them present
/// and all of them recognised.
#[derive(Debug)]
struct Contract {
    role: Role,
    /// `None` for [`Role::None`], which has no `dl` to make claims about.
    installed: Option<Installed>,
}

/// What the environment says about the devlaunch it put on PATH.
#[derive(Debug)]
struct Installed {
    expect: Expect,
    unsaved: Shape,
    /// The exact version `pixi.toml` pinned. `None` only for [`Role::Latest`],
    /// which deliberately has no number written down.
    pinned: Option<String>,
}

impl Contract {
    /// Read all four variables, or panic saying which one is wrong.
    ///
    /// Absent and unrecognised are both fatal, and the pin's presence is tied
    /// to the role rather than left optional: a pinned environment that lost
    /// its `WF_CONTRACT_DL` would otherwise skip the only check that the
    /// version installed is the version named, and `latest` carrying one would
    /// be a number nobody updates when the lock moves.
    fn read() -> Contract {
        let role = match promised(ROLE).as_str() {
            "none" => Role::None,
            "floor" => Role::Floor,
            "latest" => Role::Latest,
            "stale" => Role::Stale,
            other => panic!("{ROLE} says {other:?}, which is not an environment in pixi.toml"),
        };
        if role == Role::None {
            // The absent case is declared by *silence*, and the silence is
            // checked: an environment that inherited another's activation block
            // would set these, and every assertion below would then be about a
            // machine that does not exist.
            for var in [EXPECT, VERSION, UNSAVED] {
                assert!(
                    std::env::var_os(var).is_none(),
                    "the `none` environment set {var}, but it installs no `dl` \
                     for that to be true of"
                );
            }
            return Contract {
                role,
                installed: None,
            };
        }
        let expect = match promised(EXPECT).as_str() {
            "usable" => Expect::Usable,
            "too-old" => Expect::TooOld,
            other => panic!("{EXPECT} says {other:?}; it is `usable` or `too-old`"),
        };
        let unsaved = match promised(UNSAVED).as_str() {
            "object" => Shape::Object,
            "sentence" => Shape::Sentence,
            other => panic!("{UNSAVED} says {other:?}; it is `object` or `sentence`"),
        };
        let pinned = std::env::var(VERSION).ok();
        match (role, &pinned) {
            (Role::Latest, Some(v)) => panic!(
                "the `latest` environment pinned {v} in {VERSION}. It installs \
                 whatever pixi.lock resolved, and a number written beside that \
                 is one nobody updates when the lock moves."
            ),
            (Role::Floor | Role::Stale, None) => panic!(
                "the `{role:?}` environment pins an exact devlaunch but set no \
                 {VERSION}, so nothing checks that the version installed is the \
                 version named."
            ),
            _ => {}
        }
        Contract {
            role,
            installed: Some(Installed {
                expect,
                unsaved,
                pinned,
            }),
        }
    }

    /// The installed devlaunch, or `None` in the environment that has none.
    ///
    /// Tests that need a `dl` bail on `None`. That is a skip, and skips are
    /// what this file spent a review round removing — the difference is that
    /// the value skipped on is a validated enum rather than a string nobody
    /// checked, `Role::None` is itself asserted (no `dl` is reachable, see
    /// [`the_environment_without_devlaunch_really_has_none`]), and every test
    /// below does real work in at least one of the other three.
    fn installed(&self) -> Option<&Installed> {
        self.installed.as_ref()
    }
}

/// The panic for a run outside the pixi environments.
///
/// Spelled out rather than left as a missing-variable unwrap, because the
/// obvious thing to type is `cargo test --test live_devlaunch` and the obvious
/// reading of a failure there is "the contract is broken", which it is not.
fn only_under_pixi(var: &str) -> String {
    format!(
        "{var} is not set, so there is nothing to hold this `dl` to.\n\
         This file runs a real devlaunch at a version pixi chose:\n\
         \n\
         \x20   pixi run -e floor  contract\n\
         \x20   pixi run -e latest contract\n\
         \x20   pixi run -e stale  contract\n\
         \n\
         Running it under a bare `cargo test` would test whichever `dl` happens \
         to be installed, which is what this file exists not to do."
    )
}

fn promised(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| panic!("{}", only_under_pixi(var)))
}

/// The `dl` this run is about: the first one on PATH, which under `pixi run` is
/// the environment's.
///
/// Resolved here rather than by handing `Command` a bare name, because *which
/// file* answered is itself one of the assertions — see
/// [`the_dl_under_test_is_the_one_the_environment_installed`].
fn dl() -> PathBuf {
    let path = std::env::var_os("PATH").expect("a PATH");
    std::env::split_paths(&path)
        .map(|dir| dir.join("dl"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no `dl` on PATH, but {ROLE} says this is the `{}` environment. \
                 Is this the `default` one?",
                promised(ROLE)
            )
        })
}

/// A scratch directory under the temp dir, keyed by pid.
///
/// Not cleaned up on a panic, deliberately: a failing contract test is a thing
/// somebody is about to go and look at, and what `dl` wrote is the evidence.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wf-contract-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Run the `dl` under test and return its stdout, or panic with its stderr.
fn ask_dl(args: &[&str], what: &str) -> String {
    let program = dl();
    let out = Command::new(&program)
        .args(args)
        .env("HOME", scratch(what))
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", program.display()));
    assert!(
        out.status.success(),
        "`dl {}` failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What `wf` makes of the installed `dl`.
fn verdict() -> Devlaunch {
    Devlaunch::from_version_output(&ask_dl(&["--version"], "version"))
}

/// The Python interpreter beside the `dl` under test.
///
/// Beside it, not the first on PATH: devlaunch is a Python package, and the
/// only interpreter that can import *this* devlaunch is the one in the same
/// prefix. A host `python` would either fail to import it or — worse — import a
/// different copy and answer for a release nobody asked about.
fn python_of(program: &Path) -> PathBuf {
    let bin = program.parent().expect("dl lives in a directory");
    let python = bin.join("python");
    assert!(
        python.is_file(),
        "no interpreter beside {} — this test needs the devlaunch package, \
         not only the `dl` entry point",
        program.display()
    );
    python
}

/// Run a snippet against the devlaunch under test and return its stdout.
///
/// Reaching past the CLI into the package is deliberate rather than an
/// oversight, and it is the only way to reach two of the things worth pinning:
/// the CLI cannot be made to emit a `wouldLose` row without a real workspace
/// and a devpod daemon. The coupling is honest — a failure here says devlaunch
/// moved something that defines this contract, which is a thing `wf` needs to
/// be told rather than to find out from a user.
fn ask_devlaunch(snippet: &str, args: &[&str], why: &str) -> String {
    let program = dl();
    let python = python_of(&program);
    let out = Command::new(&python)
        .arg("-c")
        .arg(snippet)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", python.display()));
    assert!(
        out.status.success(),
        "{why}:\n{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One row of a listing, carrying `unsaved` exactly as devlaunch wrote it.
///
/// Read back through the real parser rather than through a `serde_json` call on
/// the field alone: `unsaved` is read as part of a `Workspace`, and a row is
/// what `dl` actually emits.
fn read_back(unsaved: &str) -> Option<Unsaved> {
    let row = format!(
        r#"[{{"id":"ws","devlaunch":true,"repo":"blooop/wayfinder",
              "branch":"wayfinder/wayfinder-1","state":"Stopped",
              "unsaved":{unsaved}}}]"#
    );
    let parsed = parse_workspaces(row.as_bytes())
        .unwrap_or_else(|e| panic!("`wf` could not read {unsaved}: {e:#}"));
    parsed.into_iter().next().expect("one row").unsaved
}

#[test]
fn the_dl_under_test_is_the_one_the_environment_installed() {
    // `pixi run` prepends its prefix to the *inherited* PATH, so a developer's
    // own `~/.pixi/bin/dl` is still on it, several entries later. If the
    // environment's own devlaunch were ever missing — a feature not listed on
    // the environment, a solve that dropped it — every other test in this file
    // would carry on happily against the host's install and report a verdict
    // about a version nobody selected. This is the assertion that makes the
    // pin mean something.
    let contract = Contract::read();
    if contract.installed().is_none() {
        return;
    }
    let program = dl();
    let prefix = std::env::var("CONDA_PREFIX")
        .unwrap_or_else(|_| panic!("{}", only_under_pixi("CONDA_PREFIX")));
    assert!(
        program.starts_with(&prefix),
        "the first `dl` on PATH is {}, which is outside this pixi environment \
         ({prefix}) — the version under test is not the one pixi installed",
        program.display()
    );
}

#[test]
fn wf_can_read_the_version_this_dl_prints() {
    // `Devlaunch::Unreadable` is the arm for a `--version` this binary cannot
    // parse, and it sends every isolated launch to the host. It has never been
    // reached by anything but a string written in this repository. A devlaunch
    // that reformatted its version banner would land here and nowhere else.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let printed = ask_dl(&["--version"], "version");
    assert_ne!(
        Devlaunch::from_version_output(&printed),
        Devlaunch::Unreadable,
        "`wf` could not read a version out of what this `dl` prints: {printed:?}"
    );

    let got = verdict();
    match installed.expect {
        Expect::Usable => assert_eq!(
            got,
            Devlaunch::Usable,
            "this dl ({}) should clear the {DEVLAUNCH_FLOOR} floor",
            printed.trim()
        ),
        Expect::TooOld => assert!(
            matches!(got, Devlaunch::TooOld(_)),
            "this dl ({}) is below the {DEVLAUNCH_FLOOR} floor and should read \
             as TooOld, not {got:?}",
            printed.trim()
        ),
    }
}

#[test]
fn the_floor_environment_is_pinned_to_the_floor() {
    // The claim AGENTS.md makes, stated as an assertion rather than as a guard.
    //
    // An earlier draft wrote this as `if pinned == DEVLAUNCH_FLOOR { ... }`,
    // which can only fire when the two already agree — so lowering the constant
    // to 0.0.23 while pixi still pinned 0.0.24 left all seven tests green, and
    // the `floor` environment was no longer running at the floor. That is the
    // exact condition this file exists to prevent, so the role is asked
    // explicitly and the comparison is unconditional.
    //
    // Note what is *not* asserted: that `DEVLAUNCH_FLOOR` equals
    // `UNSAVED_IS_AN_OBJECT`. The draft did, and it was actively dangerous —
    // `src/launch.rs` documents those two as separate facts that a floor bump
    // must be able to part, and the only way to satisfy such an assertion after
    // a legitimate bump is to raise `UNSAVED_IS_AN_OBJECT` too, which makes
    // `answers_unsaved` false for a real 0.0.24 and walks `wf reap` straight
    // back into devlaunch#171.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let Some(pinned) = installed.pinned.as_deref() else {
        assert_eq!(contract.role, Role::Latest, "only `latest` may omit a pin");
        return;
    };
    let printed = ask_dl(&["--version"], "version");
    let printed = printed.trim();
    assert!(
        printed.split_whitespace().any(|word| word == pinned),
        "pixi pinned devlaunch {pinned} but the `dl` on PATH says {printed:?}"
    );
    if contract.role == Role::Floor {
        assert_eq!(
            pinned,
            DEVLAUNCH_FLOOR.to_string(),
            "the `floor` environment installs devlaunch {pinned}, but the floor \
             is {DEVLAUNCH_FLOOR}. Whichever of the two moved, the other has to \
             move with it — a floor nothing is ever run at is a floor nobody \
             has checked."
        );
    }
}

#[test]
fn a_listing_from_a_real_dl_is_one_wf_can_read() {
    // `parse_workspaces` is all-or-nothing: a listing with one field `wf`
    // cannot read is a listing it reads *none* of, and `wf reap` then collects
    // nothing and says so in a way that looks like "there is nothing to do".
    // The listing here is empty — a scratch `HOME` has no workspaces — so what
    // this pins is the envelope: that `--ls --json` still exists, still exits
    // zero without a daemon, and still answers with a JSON array rather than a
    // banner, a wrapper object, or a line of prose. The rows themselves are
    // reached below, from the emitter rather than from an instance.
    let contract = Contract::read();
    if contract.installed().is_none() {
        return;
    }
    let body = ask_dl(&["--ls", "--json"], "listing");
    let parsed = parse_workspaces(body.as_bytes())
        .unwrap_or_else(|e| panic!("`wf` could not read this dl's listing ({e:#}): {body:?}"));
    assert!(
        parsed.is_empty(),
        "a scratch HOME reported workspaces, so this ran against a real \
         machine's devpod: {parsed:?}"
    );
}

#[test]
fn every_unsaved_answer_this_dl_can_give_is_one_wf_reads() {
    // The field that has already broken once, asked of the code that writes it.
    //
    // `wf`'s fixtures spell these three keys because somebody read devlaunch's
    // documentation and typed them out. This asks devlaunch's own
    // `unsaved_as_json` to produce them instead, so a rename on that side is a
    // failure here rather than a `wf reap` that silently refuses every
    // workspace on every machine.
    let contract = Contract::read();
    if contract.installed().map(|i| i.unsaved) != Some(Shape::Object) {
        return;
    }
    let emitted = ask_devlaunch(
        r#"
import json
from devlaunch.workspace_state import (
    unsaved_as_json, NothingToLose, WouldLose, CouldNotTell,
)
for answer in (
    NothingToLose(),
    WouldLose("2 uncommitted change(s) (pixi.lock, notes.md)"),
    CouldNotTell("fatal: not a git repository"),
):
    print(json.dumps(unsaved_as_json(answer)))
"#,
        &[],
        "devlaunch could not be asked what it writes for `unsaved` — it has \
         probably moved or renamed `workspace_state.unsaved_as_json`, which is \
         the function that defines this contract",
    );
    let answers: Vec<&str> = emitted.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        answers.len(),
        3,
        "expected one line per arm, got {emitted:?}"
    );

    let expected = [
        Unsaved::NothingToLose,
        Unsaved::WouldLose("2 uncommitted change(s) (pixi.lock, notes.md)".to_string()),
        Unsaved::CouldNotTell("fatal: not a git repository".to_string()),
    ];
    for (answer, want) in answers.iter().zip(expected) {
        assert_eq!(
            read_back(answer).as_ref(),
            Some(&want),
            "devlaunch writes {answer} and `wf` read it as something else"
        );
    }
}

#[test]
fn what_this_dl_says_about_a_clone_holding_work_is_what_wf_reads() {
    // The whole path, on both sides of the version boundary: a real checkout
    // with a real uncommitted file, inspected by *this* devlaunch's own
    // `read_clone`, serialised the way *this* release serialises it, and read
    // back by `wf`. Whatever comes out, `wf` has to arrive at "there is work
    // here" — that is the answer standing between `wf reap` and somebody's
    // afternoon.
    //
    // This is also what pins `WF_CONTRACT_UNSAVED` to the release rather than
    // to a comment. An earlier draft checked the 0.0.23 side by asserting that
    // `from devlaunch.workspace_state import unsaved_as_json` *failed*, which
    // is true whenever anything at all is wrong with the interpreter, the
    // prefix or the package — a test that passes for reasons unrelated to its
    // subject. The shape is now read off a value the release actually produced.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let clone = scratch("dirty-clone");
    let _ = std::fs::remove_dir_all(&clone);
    std::fs::create_dir_all(&clone).expect("a clone directory");
    git(&clone, &["init", "-q"]);
    git(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
    std::fs::write(clone.join("scratch.txt"), "half-finished\n").expect("uncommitted work");

    let reported = ask_devlaunch(
        r#"
import json, sys
from pathlib import Path
from devlaunch.workspace_state import read_clone
state = read_clone(Path(sys.argv[1]))
try:
    from devlaunch.workspace_state import unsaved_as_json
    wire = unsaved_as_json(state.unsaved)
except ImportError:
    # devlaunch <= 0.0.23: the field was the bare sentence, with no
    # serialiser between the value and the listing.
    wire = state.unsaved
print(json.dumps({
    "wire": wire,
    "shape": "object" if isinstance(wire, dict) else "sentence",
}))
"#,
        &[&clone.to_string_lossy()],
        "devlaunch could not be asked what a clone holds — `read_clone` is the \
         function whose answer becomes the `unsaved` field",
    );
    let reported: serde_json::Value =
        serde_json::from_str(reported.trim()).expect("devlaunch's answer as JSON");

    let shape = match reported["shape"].as_str() {
        Some("object") => Shape::Object,
        Some("sentence") => Shape::Sentence,
        other => panic!("devlaunch reported an unknown shape: {other:?}"),
    };
    assert_eq!(
        shape, installed.unsaved,
        "this devlaunch writes `unsaved` as a {shape:?}, but {UNSAVED} in \
         pixi.toml says {:?}. {} is where `wf` believes the object form began.",
        installed.unsaved, UNSAVED_IS_AN_OBJECT
    );

    let wire = reported["wire"].to_string();
    let read = read_back(&wire);
    let Some(Unsaved::WouldLose(said)) = read else {
        panic!(
            "devlaunch says this clone holds work ({wire}), and `wf` read that \
             as {read:?} — anything but WouldLose here is a workspace `wf reap` \
             would be willing to delete"
        );
    };
    assert!(
        said.contains("uncommitted"),
        "the sentence survived the round trip but says nothing about what is \
         at risk: {said:?}"
    );
}

#[test]
fn the_verbs_wf_hands_dl_are_ones_it_still_knows() {
    // `wf` builds two argvs it never gets to try here: `dl <ws> up` (prewarm)
    // and `dl <id> rm [--force]` (reap). Both change the machine and both need
    // a devpod daemon, so running them is devlaunch's own e2e suite's job — but
    // *not checking them at all* is how the 0.14.0 incident happened, where
    // `wf` shipped a call to an `up` the installed release had never heard of.
    //
    // `dl <bogus> up` cannot stand in for it: `dl` resolves the workspace
    // before it looks at the verb, so an unknown workspace and an unknown verb
    // produce the same error. What is left is the vocabulary — `dl --help` is
    // where devlaunch documents its own subcommands, and a verb that is removed
    // stops being named there. It catches a deletion or a rename, which is the
    // failure that has actually happened, and it does not pretend to catch a
    // change in what the verb does.
    // The two verbs are not asked for on the same terms, and the difference is
    // the floor's whole justification. `rm` is wanted from every release: reap
    // reads and deletes through whichever `dl` is on PATH, floor or no floor.
    // `up` is wanted only at or above the floor — it *arrived* in 0.0.24, which
    // is why `DEVLAUNCH_FLOOR` is that number. Below the floor its absence is
    // therefore asserted rather than tolerated, and that assertion is the only
    // place in this repo where the reason the floor exists is evidence instead
    // of a sentence in a doc comment. (It also caught a first draft of this very
    // test, which demanded `up` of 0.0.23 and was wrong to.)
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let help = ask_dl(&["--help"], "help");
    let names = |verb: &str| {
        help.split_whitespace()
            .any(|word| word.trim_matches(',') == verb)
    };
    assert!(
        names("rm"),
        "`dl --help` no longer names `rm`, which `src/reap.rs` hands it on \
         every release:\n{help}"
    );
    match installed.expect {
        Expect::Usable => assert!(
            names("up"),
            "`dl --help` no longer names `up`, which `src/launch.rs` hands it \
             for every prewarm. That verb is the {DEVLAUNCH_FLOOR} floor's \
             subject: a `dl` clearing the floor must carry it.\n{help}"
        ),
        Expect::TooOld => assert!(
            !names("up"),
            "this `dl` is below the {DEVLAUNCH_FLOOR} floor and yet names \
             `up`, so the floor is not where `up` arrived and the number is \
             wrong:\n{help}"
        ),
    }
}

#[test]
fn whether_wf_trusts_a_missing_unsaved_follows_the_release() {
    // The decision `answers_unsaved` exists for, and the most dangerous one in
    // the pair: on a release that answers for every clone of its own, a `null`
    // beside `devlaunch: true` means devlaunch's inspection fell over, and
    // reaping on it destroys work. On a release that does not, the same `null`
    // is the ordinary clean case and refusing it would break `wf reap`
    // entirely. Nothing but the version can tell them apart, so the two facts
    // are asserted against each other here rather than each against a fixture.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let object = installed.unsaved == Shape::Object;
    assert_eq!(
        verdict().answers_unsaved(),
        object,
        "this dl {} the object form, but `wf` {} it to answer `unsaved` for \
         every clone it made",
        if object { "writes" } else { "predates" },
        if object { "does not expect" } else { "expects" }
    );
}

/// A git command in the scratch clone, with an identity of its own.
///
/// `-c user.*`: the machine running this may have no global git identity (CI
/// does not), and a `commit` that fails for that reason would look like the
/// clone being unreadable.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=contract@example.invalid"])
        .args(["-c", "user.name=contract"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run git: {e}"));
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

#[test]
fn the_environment_without_devlaunch_really_has_none() {
    // The premise every assertion in the `none` environment rests on, and the
    // one that is easiest to lose: `pixi run` prepends its prefix to the
    // *inherited* PATH, so an environment that installs no devlaunch still sees
    // the developer's own. `scripts/without-dl.sh` is what takes it away, and
    // this is where its failure would surface as a failed test rather than as
    // every check below quietly passing against a `dl` nobody chose.
    if Contract::read().role != Role::None {
        return;
    }
    let path = std::env::var_os("PATH").expect("a PATH");
    let found: Vec<PathBuf> = std::env::split_paths(&path)
        .map(|dir| dir.join("dl"))
        .filter(|candidate| candidate.is_file())
        .collect();
    assert!(
        found.is_empty(),
        "the `none` environment still has a `dl` on PATH: {found:?}. Either the \
         task is not going through scripts/without-dl.sh, or the scrubber has \
         stopped working."
    );
}

#[test]
fn a_devcontainer_repo_is_isolated_only_when_a_real_dl_can_carry_it() {
    // The fallback itself, rather than the reading that decides it.
    //
    // `Isolation::detect` is the function that answers "does this launch go
    // into a container or stay on the host", and until now it had only ever
    // been asked on a machine whose `dl` was a shell script this repo wrote.
    // This asks it in a checkout that really does carry a
    // `.devcontainer/devcontainer.json` — this one — against a `dl` that really
    // is absent, really is 0.0.23, or really is the floor.
    //
    // The two `Host` answers are the point. A `dl` that is missing, and a `dl`
    // that is too old, both have to end in an ordinary host launch: #80's rule
    // is that a repo may carry a devcontainer for its editor users without that
    // conscripting `wf` into a container it cannot actually produce.
    let contract = Contract::read();
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        here.join(".devcontainer/devcontainer.json").is_file(),
        "this test is only meaningful in a checkout that declares a \
         devcontainer, and {} no longer does",
        here.display()
    );
    let got = Isolation::detect(here, Agent::Claude);
    let want = match contract.installed().map(|i| i.expect) {
        Some(Expect::Usable) => Isolation::Devlaunch,
        // Absent, or on PATH and below the floor.
        None | Some(Expect::TooOld) => Isolation::Host,
    };
    assert_eq!(
        got,
        want,
        "a devcontainer checkout with {} resolved to {got:?}",
        match contract.role {
            Role::None => "no dl at all".to_string(),
            _ => format!("dl {}", ask_dl(&["--version"], "version").trim()),
        }
    );
}

#[test]
fn a_launch_that_fell_back_says_why_and_names_the_version() {
    // The half of a degradation a missing `(devlaunch)` suffix cannot carry.
    //
    // A `wf` that silently runs on the host looks exactly like a `wf` that
    // ignored the devcontainer, and the difference is a fixable install. The
    // sentence is built from the version `dl` actually printed, so this is
    // where it is checked against one.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        // Absent is deliberately silent — #80 again: a line on every launch
        // would be noise about a tool the user never asked for. That arm is
        // asserted in `src/launch.rs`, where no `dl` is needed to state it.
        return;
    };
    let said = verdict().shortfall();
    match installed.expect {
        Expect::Usable => assert_eq!(
            said, None,
            "a usable dl has nothing to explain, but the notice would say: {said:?}"
        ),
        Expect::TooOld => {
            let said = said.expect(
                "a dl below the floor sends the launch to the host, and the \
                 notice is the only thing that says so",
            );
            let printed = ask_dl(&["--version"], "version");
            let version = printed.split_whitespace().nth(1).unwrap_or_default();
            assert!(
                said.contains(version) && said.contains(&DEVLAUNCH_FLOOR.to_string()),
                "the notice should name both the version found and the floor \
                 it missed, and says: {said:?}"
            );
            assert!(
                said.contains("ran on the host"),
                "the notice should say what happened instead: {said:?}"
            );
        }
    }
}

#[test]
fn a_reap_with_no_dl_refuses_instead_of_guessing() {
    // `wf reap` is the one path in this crate that deletes, and it decides what
    // to delete from what `dl --ls --json` told it. With no `dl` there is no
    // listing, and the only safe reading of "no listing" is *stop* — an empty
    // one would mean "no workspaces exist", which is the same bytes and the
    // opposite fact.
    //
    // The real binary, because that is the only way to see the exit status and
    // the message a user gets; `reap::workspaces` returning an `Err` in-process
    // says nothing about whether `main` treats it as fatal.
    if Contract::read().role != Role::None {
        return;
    }
    let home = scratch("reap-home");
    let out = Command::new(env!("CARGO_BIN_EXE_wf"))
        .args(["reap", "-y"])
        .env("HOME", &home)
        .output()
        .expect("the wf binary");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "`wf reap -y` with no devlaunch exited {} — a reap that cannot see the \
         workspaces must fail rather than report success over an empty plan:\n{said}",
        out.status
    );
    assert!(
        said.contains("devlaunch") && said.contains("PATH"),
        "the failure should say what is missing and where it was looked for, \
         and says:\n{said}"
    );
    assert!(
        !said.contains("nothing to reap"),
        "`wf` reported an empty plan when it simply could not see: {said}"
    );
}

// ---------------------------------------------------------------------------
// The two calls that change the machine, run for real with `devpod` shimmed.
//
// `dl <ws> rm` and `dl <ws> up` need a devpod daemon and a container, so this
// file used to check only that `dl --help` still named them. It does not have
// to stop there. `dl`'s single devpod spawn is `subprocess.run(["devpod", ...])`
// — a bare name, resolved on PATH — so putting a recording `devpod` in front of
// the *real* `dl` runs devlaunch's own argument parsing, its own workspace
// resolution and its own decision about what to ask devpod, and stops at the
// point where a container would be built.
//
// That is the opposite of the shim this whole file exists to get away from.
// `src/probe.rs` shims `dl` itself, which is why its fixtures could drift; here
// the real `dl` is the subject and the shim is one layer *below* it, standing in
// for the daemon rather than for the program under test.
// ---------------------------------------------------------------------------

/// The workspace the shimmed devpod claims to have.
///
/// Shaped like one `wf` makes, because `dl` derives things from the name.
const SHIMMED_WORKSPACE: &str = "devlaunch-wayfinder-wayfinder-137-abcdefgh";

/// What the real `dl` asked devpod to do, and what it said while doing it.
#[derive(Debug)]
struct Asked {
    /// One line per devpod invocation: the program and each argument in angle
    /// brackets, so an argument containing a space cannot read as two.
    devpod: Vec<String>,
    status: std::process::ExitStatus,
    said: String,
}

impl Asked {
    /// The one devpod call whose first argument is `verb`, or `None`.
    fn call(&self, verb: &str) -> Option<&String> {
        self.devpod
            .iter()
            .find(|line| line.starts_with(&format!("devpod <{verb}>")))
    }
}

/// Run the real `dl` with a recording `devpod` in front of it.
///
/// `argv` is what `wf` itself builds — `reap::removal_argv`,
/// `launch::prewarm_argv`, `launch::isolated_argv` — with a leading `dl`
/// stripped if it carries one. Nothing here spells an argument out.
fn dl_over_a_shimmed_devpod(argv: &[String], what: &str) -> Asked {
    let program = dl();
    let dir = scratch(&format!("devpod-{what}"));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("a scratch home");
    let log = dir.join("devpod.log");
    std::fs::write(&log, "").expect("the log");

    // Records and answers; runs nothing. `dl <ws> up` hands devpod a
    // `--command` carrying a shell script that would install tooling — this
    // shim must never be the thing that executes it, which is why every arm
    // below either echoes a fixture or does nothing at all.
    let shim = dir.join("devpod");
    std::fs::write(
        &shim,
        format!(
            r#"#!/bin/sh
printf 'devpod' >> "$DP_LOG"
for a in "$@"; do printf ' <%s>' "$a" >> "$DP_LOG"; done
echo >> "$DP_LOG"
case "$1" in
  version) echo "v0.26.1" ;;
  list) echo '[{{"id":"{SHIMMED_WORKSPACE}","source":{{"localFolder":"/nonexistent"}},"provider":{{"name":"docker"}},"ide":{{"name":"none"}},"lastUsed":"2026-08-08T11:43:27Z"}}]' ;;
  status) echo '{{"state":"Stopped"}}' ;;
  context) echo '{{}}' ;;
esac
exit 0
"#
        ),
    )
    .expect("the devpod shim");
    let mut perms = std::fs::metadata(&shim).expect("the shim").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&shim, perms).expect("an executable shim");

    let after_program: Vec<&String> = argv
        .iter()
        .skip(usize::from(argv.first().is_some_and(|a| a == "dl")))
        .collect();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(&program)
        .args(&after_program)
        .env("PATH", path)
        .env("HOME", &home)
        .env("DP_LOG", &log)
        // Cleared, not inherited. With a token in the environment `dl` forwards
        // it to devpod as `--init-env GH_TOKEN=...`, which would put a real
        // credential into the recording below — and this test prints that
        // recording when it fails.
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", program.display()));

    Asked {
        devpod: std::fs::read_to_string(&log)
            .expect("the log")
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        status: out.status,
        said: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn the_removal_wf_sends_reaches_devpod_as_a_delete() {
    // `wf reap`'s argv, run by the real `dl`, on every release: reap deletes
    // through whichever `dl` is on PATH, so this has to hold below the floor
    // too.
    let contract = Contract::read();
    if contract.installed().is_none() {
        return;
    }
    let argv = wf::reap::removal_argv(SHIMMED_WORKSPACE, true);
    let asked = dl_over_a_shimmed_devpod(&argv, "rm");
    assert!(
        asked.status.success(),
        "`dl {}` failed ({}):\n{}",
        argv.join(" "),
        asked.status,
        asked.said
    );
    let deleted = asked.call("delete").unwrap_or_else(|| {
        panic!(
            "`dl {}` never asked devpod to delete anything. It asked:\n{:#?}\n{}",
            argv.join(" "),
            asked.devpod,
            asked.said
        )
    });
    assert!(
        deleted.contains(&format!("<{SHIMMED_WORKSPACE}>")),
        "the delete names a different workspace: {deleted}"
    );
}

#[test]
fn the_prewarm_wf_sends_is_the_verb_the_floor_exists_for() {
    // The 0.14.0 regression, as a test.
    //
    // `wf` 0.14.0 shipped `dl <workspace> up` while the released devlaunch was
    // 0.0.23, which had no `up`. It failed inside a detached prewarm nobody was
    // watching, and neither repository's CI noticed. `DEVLAUNCH_FLOOR` exists
    // because of it. Here the same argv goes to a real 0.0.23 and a real
    // 0.0.24, and the two answers are asserted apart: one reaches `devpod up`,
    // the other is refused by `dl`'s own argument parsing before devpod is
    // asked anything at all.
    let contract = Contract::read();
    let Some(installed) = contract.installed() else {
        return;
    };
    let argv = wf::launch::prewarm_argv(SHIMMED_WORKSPACE);
    let asked = dl_over_a_shimmed_devpod(&argv, "up");
    match installed.expect {
        Expect::Usable => {
            assert!(
                asked.status.success(),
                "`{}` failed on a dl at or above the floor ({}):\n{}",
                argv.join(" "),
                asked.status,
                asked.said
            );
            let up = asked.call("up").unwrap_or_else(|| {
                panic!(
                    "`{}` never reached `devpod up`. It asked:\n{:#?}\n{}",
                    argv.join(" "),
                    asked.devpod,
                    asked.said
                )
            });
            assert!(
                up.contains(&format!("<{SHIMMED_WORKSPACE}>")),
                "the prewarm brought up a different workspace: {up}"
            );
        }
        Expect::TooOld => {
            assert!(
                !asked.status.success(),
                "`{}` succeeded on a dl below the {DEVLAUNCH_FLOOR} floor, so \
                 either `up` predates it and the floor is wrong, or this dl \
                 accepted a verb it does not have:\n{}",
                argv.join(" "),
                asked.said
            );
            assert!(
                asked.call("up").is_none(),
                "a dl below the floor still reached `devpod up`: {:#?}",
                asked.devpod
            );
        }
    }
}

#[test]
fn an_isolated_launch_arrives_as_one_shell_command() {
    // `wf`'s quoting against `dl`'s shell, with nothing between them.
    //
    // `dl <ws> -- <cmd>` joins everything after `--` and runs it through a
    // shell inside the container, so `wf` single-quotes each argument and sends
    // one entry. An unquoted prompt arrives as several arguments and the agent
    // is launched with the wrong argv — and a prompt is the one thing here that
    // always contains spaces. This is checked against the real `dl` because the
    // splitting happens on its side of the boundary.
    let contract = Contract::read();
    if contract.installed().map(|i| i.expect) != Some(Expect::Usable) {
        return;
    }
    let agent = [
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "/wf-one blooop/wayfinder 137".to_string(),
    ];
    let argv = wf::launch::isolated_argv(SHIMMED_WORKSPACE, &agent);
    let asked = dl_over_a_shimmed_devpod(&argv, "exec");
    assert!(
        asked.status.success(),
        "`{}` failed ({}):\n{}",
        argv.join(" "),
        asked.status,
        asked.said
    );
    // The last `devpod ssh --command` is the agent; the earlier ones are
    // devlaunch's own tool probes.
    let ran = asked
        .devpod
        .iter()
        .rfind(|line| line.starts_with("devpod <ssh>"))
        .unwrap_or_else(|| {
            panic!(
                "the launch never reached the container. It asked:\n{:#?}\n{}",
                asked.devpod, asked.said
            )
        });
    let command = ran
        .rsplit_once("<--command> <")
        .and_then(|(_, rest)| rest.strip_suffix('>'))
        .unwrap_or_else(|| panic!("no `--command` in the ssh call: {ran}"));

    // Not asserted as a string. How `dl` escapes this for its own shell is
    // `dl`'s business and it re-quotes what `wf` already quoted; pinning the
    // spelling would fail on a change that is none of `wf`'s concern. What `wf`
    // depends on is what the *agent* ends up being called with, so the command
    // is run — in a real shell, with `claude` replaced by something that writes
    // its argv down. This is the only place the quoting can actually be
    // checked, and it is checked on the far side of `dl`.
    let bin = scratch("agent-argv");
    let _ = std::fs::remove_dir_all(&bin);
    std::fs::create_dir_all(&bin).expect("a scratch bin");
    let seen = bin.join("argv");
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '<%s>' \"$a\" >> '{}'; done\n",
            seen.display()
        ),
    )
    .expect("the claude recorder");
    let mut perms = std::fs::metadata(&claude)
        .expect("the recorder")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&claude, perms).expect("an executable recorder");

    let out = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &bin)
        .output()
        .expect("a shell");
    assert!(
        out.status.success(),
        "the command `dl` would run in the container is not one a shell \
         accepts ({}): {command}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let got = std::fs::read_to_string(&seen).unwrap_or_default();
    assert_eq!(
        got, "<--dangerously-skip-permissions></wf-one blooop/wayfinder 137>",
        "the agent was called with the wrong argv. The prompt has to arrive as \
         one argument; three is what an unquoted join produces.\nfrom: {command}"
    );
}
