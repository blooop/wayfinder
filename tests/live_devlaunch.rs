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
//! no `WF_CONTRACT_*` in the environment every test that goes near a `dl`
//! panics saying so, rather than quietly testing whichever one the developer
//! happens to have. (The two guards at the bottom of the file are the
//! exception, and are meant to be: one reads this file's source and one reads a
//! child's environment, and neither needs a devlaunch to be right about.)
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
//! One limit, because the argv is only half `wf`'s: the *verbs* and flags come
//! from `wf`'s own builders, but the workspace argument is a devpod id rather
//! than the `owner/repo@wayfinder/<repo>-<n>` spec a real launch passes. The id
//! sends `dl` down its resolve-an-existing-workspace branch; the spec form
//! would have it clone, which needs a network and a credential and is
//! devlaunch's own e2e suite's business. So a devlaunch release that changed
//! how it *parses or clones* a `repo@branch` spec is not caught here.
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
    let out = hermetic(&program, &scratch(what))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", program.display()));
    // Everything captured from a real `dl` is checked before it can be
    // printed, not only the shimmed recording: these panics quote stderr
    // verbatim and they run in a public CI log.
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    let complained = String::from_utf8_lossy(&out.stderr).into_owned();
    refuse_tokens("what dl printed", &printed);
    refuse_tokens("what dl said", &complained);
    assert!(
        out.status.success(),
        "`dl {}` failed ({}): {}",
        args.join(" "),
        out.status,
        complained.trim()
    );
    printed
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
    let out = hermetic(&python, &scratch("devlaunch-ask"))
        .arg("-c")
        .arg(snippet)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", python.display()));
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    let complained = String::from_utf8_lossy(&out.stderr).into_owned();
    refuse_tokens("what devlaunch printed", &printed);
    refuse_tokens("what devlaunch said", &complained);
    assert!(out.status.success(), "{why}:\n{}", complained.trim());
    printed
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
    let out = hermetic(Path::new("git"), dir)
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
    let out = hermetic(Path::new(env!("CARGO_BIN_EXE_wf")), &home)
        .args(["reap", "-y"])
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

/// One recorded devpod invocation, parsed into arguments.
///
/// Parsed rather than kept as a line, because everything that reads a recording
/// wants a *field*: the guard below asks whether any argument is an assignment,
/// the isolated-launch test asks for the value of `--command`. Both used to do
/// it by string surgery on the whole line, and both were wrong in the same way
/// — one treated `dl`'s entire output as a single `NAME=value`, the other
/// assumed `--command` was the last argument. A parse costs four lines and
/// removes the class.
#[derive(Debug, Clone)]
struct Call {
    args: Vec<String>,
}

impl Call {
    /// `devpod <a> <b>` → `["a", "b"]`.
    fn parse(line: &str) -> Option<Call> {
        let rest = line.strip_prefix("devpod")?;
        Some(Call {
            args: rest
                .split('<')
                .skip(1)
                .filter_map(|field| field.rsplit_once('>').map(|(arg, _)| arg.to_string()))
                .collect(),
        })
    }

    fn verb(&self) -> Option<&str> {
        self.args.first().map(String::as_str)
    }

    /// The argument after `flag`, or `None` if it was not passed.
    fn value_of(&self, flag: &str) -> Option<&str> {
        let at = self.args.iter().position(|arg| arg == flag)?;
        self.args.get(at + 1).map(String::as_str)
    }

    fn mentions(&self, needle: &str) -> bool {
        self.args.iter().any(|arg| arg.contains(needle))
    }

    /// How this call reads in a failure message: every `NAME=VALUE` argument
    /// with its value elided.
    ///
    /// Display only. The assertions read [`Call::args`], which is untouched —
    /// an earlier version rewrote the recording itself and could therefore
    /// change what a test was asserting on, which is a strange way to keep a
    /// secret.
    fn shown(&self) -> String {
        let shown: Vec<String> = self
            .args
            .iter()
            .map(|arg| match arg.split_once('=') {
                Some((name, _)) if is_variable_name(name) => format!("{name}=…"),
                _ => arg.clone(),
            })
            .collect();
        format!("devpod {}", shown.join(" "))
    }
}

/// A plausible environment-variable name, and nothing else.
///
/// The distinction matters twice: `NAME=VALUE` is an assignment worth hiding,
/// while `bash -lc 'FOO=1 …'` is a command and rewriting it would corrupt the
/// thing under assertion.
fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// What the real `dl` asked devpod to do, and what it said while doing it.
#[derive(Debug)]
struct Asked {
    calls: Vec<Call>,
    status: std::process::ExitStatus,
    said: String,
}

impl Asked {
    /// The one devpod call whose verb is `verb`, or `None`.
    fn call(&self, verb: &str) -> Option<&Call> {
        self.calls.iter().find(|call| call.verb() == Some(verb))
    }

    /// Every call, rendered for a failure message with assignment values gone.
    fn shown(&self) -> String {
        self.calls.iter().fold(String::new(), |mut out, call| {
            out.push_str("  ");
            out.push_str(&call.shown());
            out.push('\n');
            out
        })
    }
}

/// Every subprocess this file starts **directly**, with an environment built
/// rather than inherited.
///
/// The inheriting version of this was two `env_remove` calls, `GH_TOKEN` and
/// `GITHUB_TOKEN`, which is a denylist of the two names that happened to be
/// true when it was written. Three things are wrong with that shape and all of
/// them are drift:
///
/// * devlaunch reads `HOST_TOKEN_VARS` *first* and falls back to running
///   `gh auth token`, so the credential need never be in the environment at
///   all — it can come from `gh`'s config or keyring.
/// * a variable added on either side is admitted by default.
/// * nothing notices when the list stops being complete.
///
/// So the environment is allowlisted: cleared, then given back the three things
/// a run needs — `PATH`, `HOME`, and devlaunch's own `DEVLAUNCH_NO_GH_TOKEN`.
/// Anything new is excluded because it was never let in. The opt-out covers the
/// sources clearing cannot reach; the scratch `HOME` covers `gh`'s config
/// directory. Neither is trusted on its own — see [`refuse_secrets`].
///
/// **Directly** is load-bearing and was once wrong here. Two tests reach `dl`
/// without going through this function, and both are meant to:
/// [`a_devcontainer_repo_is_isolated_only_when_a_real_dl_can_carry_it`] calls
/// `Isolation::detect`, and [`a_launch_that_fell_back_says_why_and_names_the_version`]
/// reads the version through the same path — each of which spawns
/// `dl --version` from inside `src/launch.rs`, with this process's whole
/// environment, because *that is what `wf` does in production* and the point of
/// those tests is the production path. `--version` is also the one `dl` call
/// that consults no credential and prints none. What must not happen is a
/// *capture* being taken from an inherited environment, and that is what this
/// function and the guard below are for.
fn hermetic(program: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        )
        .env("HOME", home)
        // devlaunch's `DISABLE_VAR`. Set so that no credential is *fetched*,
        // rather than hoping none is *inherited*.
        .env("DEVLAUNCH_NO_GH_TOKEN", "1");
    cmd
}

/// Names whose value is nobody's business, matched on shape rather than spelled
/// out — the point is to catch the variable that does not exist yet.
fn looks_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|needle| name.contains(needle))
        || name.ends_with("_KEY")
}

/// Token shapes, for a credential that arrives without a name attached.
const TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];

/// Fail if any *recorded argument* looks like a credential — before it can be
/// printed.
///
/// This is the durable half of the mitigation. devlaunch **deliberately** keeps
/// the token off the command line today: `gh_auth.up_args` writes it to a
/// private file and passes `--workspace-env-file`, and its docstring says why —
/// `devpod up` runs for minutes and its argv is visible to anyone who can run
/// `ps`. So there is nothing to catch right now. That is a decision in another
/// repository, and the reason this exists is that a change to it must surface
/// as a failed test here rather than as a token in a CI log.
///
/// Shape, not name: a rename on devlaunch's side leaves this working. The
/// message names only the key, never the value.
///
/// It takes parsed arguments rather than a line. The version that took the line
/// was also handed `dl`'s combined stdout and stderr, which contains no angle
/// brackets — so the whole blob became one field, "the name" became every byte
/// before the first `=`, and the guard would both accuse innocent output and
/// print the entire capture it exists to withhold. Free text is [`refuse_tokens`]'s
/// job and is checked for shapes only, because free text has no fields.
fn refuse_secrets(what: &str, calls: &[Call]) {
    for call in calls {
        for arg in &call.args {
            refuse_tokens(what, arg);
            let Some((name, value)) = arg.split_once('=') else {
                continue;
            };
            assert!(
                !is_variable_name(name) || !looks_secret(name) || value.is_empty(),
                "{what} carries `{name}=…` — a name shaped like a secret, with a \
                 value. The value is deliberately not shown. Either devlaunch \
                 has started putting credentials on the command line, or this \
                 needs to learn why that one is harmless."
            );
        }
    }
}

/// Fail if free text holds something shaped like a token.
///
/// The half that applies to anything captured from a real `dl` — its stderr on
/// a failed call, a listing body, a Python traceback — all of which end up in
/// a panic message and therefore in a CI log. No field parsing is possible
/// here, so this is prefixes only, and the message names the prefix rather than
/// quoting what it found.
fn refuse_tokens(what: &str, text: &str) {
    for prefix in TOKEN_PREFIXES {
        assert!(
            !text.contains(prefix),
            "{what} contains something shaped like a GitHub token (a `{prefix}` \
             string). It is deliberately not shown. This test prints what it \
             captures when it fails, so it must stop capturing this before it \
             can run again."
        );
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
    let out = hermetic(&program, &home)
        .args(&after_program)
        .env("PATH", path)
        .env("DP_LOG", &log)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", program.display()));

    let raw = std::fs::read_to_string(&log).expect("the log");
    let calls: Vec<Call> = raw.lines().filter_map(Call::parse).collect();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Both channels, each by the rule that suits it: the recording has fields,
    // so it gets the full check; `dl`'s own output does not, so it gets shapes.
    refuse_secrets("the devpod recording", &calls);
    refuse_tokens("what dl said", &said);

    Asked {
        calls,
        status: out.status,
        said,
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
            "`dl {}` never asked devpod to delete anything. It asked:\n{}{}",
            argv.join(" "),
            asked.shown(),
            asked.said
        )
    });
    assert!(
        deleted.mentions(SHIMMED_WORKSPACE),
        "the delete names a different workspace: {}",
        deleted.shown()
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
                    "`{}` never reached `devpod up`. It asked:\n{}{}",
                    argv.join(" "),
                    asked.shown(),
                    asked.said
                )
            });
            assert!(
                up.mentions(SHIMMED_WORKSPACE),
                "the prewarm brought up a different workspace: {}",
                up.shown()
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
                "a dl below the floor still reached `devpod up`:\n{}",
                asked.shown()
            );
            // *Why* it failed, not merely that it did. Without this the test
            // passes on any failure at all — a shim fixture that stopped
            // parsing, a resolution error under the scratch HOME — while the
            // regression it exists to reproduce quietly stops being reproduced.
            assert!(
                asked.said.contains("Unknown command"),
                "a dl below the floor refused the prewarm, but not by saying it \
                 does not have the verb — so this is no longer the 0.14.0 \
                 failure being reproduced:\n{}",
                asked.said
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
    // Selected by what it carries, not by where it sits.
    //
    // The first version took the *last* `devpod ssh` call, on a comment-level
    // assumption that devlaunch puts its tool probes before the agent. That
    // payload is then executed, so a reordering or an extra post-launch step on
    // devlaunch's side would have run its container bootstrap — which pipes
    // `curl` into `bash` — on this machine. It is named instead, and required
    // to be the only match.
    let agent_program = &agent[0];
    let carried: Vec<&Call> = asked
        .calls
        .iter()
        .filter(|call| {
            call.verb() == Some("ssh")
                && call
                    .value_of("--command")
                    .is_some_and(|command| command.contains(agent_program.as_str()))
        })
        .collect();
    assert_eq!(
        carried.len(),
        1,
        "expected exactly one devpod call carrying `{agent_program}`, and this \
         test runs the one it finds — so anything but one call means it does \
         not know what it would be running:\n{}",
        asked.shown()
    );
    let command = carried[0].value_of("--command").expect("matched above");

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
    let claude = bin.join(agent_program);
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '<%s>' \"$a\" >> '{}'; done\n",
            seen.display()
        ),
    )
    .expect("the agent recorder");
    let mut perms = std::fs::metadata(&claude)
        .expect("the recorder")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&claude, perms).expect("an executable recorder");

    // The command runs with a PATH holding only what it may legitimately need:
    // the recorder, and the shell `dl` asked for. Belt and braces behind the
    // selection above — if that ever picks the wrong call anyway, devlaunch's
    // bootstrap finds no `curl`, no `pixi` and no `gh`, and does nothing to
    // this machine.
    for tool in ["bash", "sh"] {
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(tool))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("no `{tool}` on PATH to run the agent command with"));
        std::os::unix::fs::symlink(real, bin.join(tool)).expect("link the shell");
    }
    let out = hermetic(&bin.join("sh"), &bin)
        .arg("-c")
        .arg(command)
        .env("PATH", &bin)
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

#[test]
fn every_subprocess_this_file_starts_itself_goes_through_hermetic() {
    // A guard on this file's own source — and only on this file's own source,
    // which is a real limit rather than an oversight. `Isolation::detect` and
    // `Devlaunch::from_version_output`'s caller both spawn `dl --version` from
    // inside `src/launch.rs`, with this process's whole environment, and they
    // are *supposed* to: those two tests exist to drive the production path,
    // and `--version` is the one `dl` call that consults no credential and
    // prints none. Nothing captured from them is printed either. What this
    // guard covers is the thing that would put a credential in a log — a
    // capture taken from an inherited environment — and that only happens
    // through a spawn written here.
    //
    // It is a guard at all because the mitigation it protects is
    // the kind that decays by addition rather than by edit: nothing about
    // writing a new test here reminds anybody that the environment has to be
    // built rather than inherited, and a run that inherits one is
    // indistinguishable from a run that does not until the day it prints a
    // token. `hermetic` is the only place allowed to start a process, so
    // "did you sanitise?" becomes "does this compile", and the answer is
    // checked rather than remembered.
    //
    // The needle is assembled rather than written, so that this test does not
    // count itself.
    let needle = concat!("Command", "::new");
    let source = include_str!("live_devlaunch.rs");

    assert_eq!(
        source.matches(needle).count(),
        1,
        "something in this file starts a subprocess without going through \
         `hermetic`, so it inherits the developer's whole environment — \
         including whatever credential is in it, in something whose output \
         this file prints when it fails. Route it through `hermetic`, or, if \
         it genuinely must inherit, say why here and change this count."
    );

    let at = source.find(needle).expect("the one spawn");
    let opens = source
        .find("fn hermetic(")
        .expect("the sanitising spawn is called `hermetic`");
    let closes = source[opens..]
        .find("\nfn ")
        .map_or(source.len(), |rel| opens + rel);
    assert!(
        (opens..closes).contains(&at),
        "the one subprocess spawn has moved out of `hermetic`. Counting it is \
         not the point; being inside the function that clears the environment is."
    );
}

#[test]
fn the_sanitised_spawn_hands_on_nothing_it_was_not_given() {
    // The allowlist, read off a real child rather than off the source.
    //
    // `hermetic` is only worth anything if it actually clears, and nothing else
    // here would notice if it stopped: every test would go on passing, quietly
    // inheriting the developer's environment again. So a child is asked what it
    // received and the answer is compared to the whole intended list — an
    // equality, not a "contains", because the failure being guarded against is
    // a variable arriving that nobody meant to send.
    //
    // `CARGO_MANIFEST_DIR` is in this process's environment (cargo puts it
    // there) and must not be in the child's. It is the canary: no variable has
    // to be planted, because the point is precisely that ambient ones do not
    // travel.
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "this test's canary is gone — it assumed cargo sets CARGO_MANIFEST_DIR \
         in the test process, and something else is needed if it no longer does"
    );
    let home = scratch("hermetic-env");
    let out = hermetic(Path::new("sh"), &home)
        .args(["-c", "env"])
        .output()
        .expect("a shell");
    assert!(
        out.status.success(),
        "could not read the child's environment"
    );

    let printed = String::from_utf8_lossy(&out.stdout);
    let mut got: Vec<&str> = printed
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name)
        // Set by the shell itself, not passed in: `sh` defines these on start
        // whatever it was handed, so they are not evidence of inheritance.
        .filter(|name| !matches!(*name, "PWD" | "SHLVL" | "_"))
        .collect();
    got.sort_unstable();
    got.dedup();
    assert_eq!(
        got,
        ["DEVLAUNCH_NO_GH_TOKEN", "HOME", "PATH"],
        "the sanitised spawn passed on a variable it was not given. Anything \
         beyond the allowlist reached the child by inheritance, which is the \
         one thing `hermetic` exists to prevent."
    );
}
