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
//! pixi run -e floor  contract   # devlaunch == launch::DEVLAUNCH_FLOOR
//! pixi run -e latest contract   # whatever pixi.lock resolved
//! pixi run -e stale  contract   # 0.0.23, below the floor
//! ```
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
//! # What it cannot reach
//!
//! `dl <id> rm` and `dl <ws> up` change the machine and need a devpod daemon, so
//! neither is run — only that this `dl` still names them is checked, in
//! [`the_verbs_wf_hands_dl_are_ones_it_still_knows`]. Everything else here is
//! read-only, under a scratch `HOME`, so it cannot see or touch a real
//! workspace. The scratch home matters for a second reason: a listing read from
//! the developer's own machine would assert against whatever they happen to
//! have cloned.

use std::path::{Path, PathBuf};
use std::process::Command;

use wf::launch::{Devlaunch, DEVLAUNCH_FLOOR, UNSAVED_IS_AN_OBJECT};
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

/// Which of the three environments this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
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
            "floor" => Role::Floor,
            "latest" => Role::Latest,
            "stale" => Role::Stale,
            other => panic!("{ROLE} says {other:?}, which is not an environment in pixi.toml"),
        };
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
            expect,
            unsaved,
            pinned,
        }
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
    let _ = Contract::read();
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
    let printed = ask_dl(&["--version"], "version");
    assert_ne!(
        Devlaunch::from_version_output(&printed),
        Devlaunch::Unreadable,
        "`wf` could not read a version out of what this `dl` prints: {printed:?}"
    );

    let got = verdict();
    match contract.expect {
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
    let Some(pinned) = contract.pinned else {
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
    let _ = Contract::read();
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
    if contract.unsaved != Shape::Object {
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
        shape, contract.unsaved,
        "this devlaunch writes `unsaved` as a {shape:?}, but {UNSAVED} in \
         pixi.toml says {:?}. {} is where `wf` believes the object form began.",
        contract.unsaved, UNSAVED_IS_AN_OBJECT
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
    match contract.expect {
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
    let object = contract.unsaved == Shape::Object;
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
