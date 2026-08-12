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
//! It is not runnable outside those environments and does not try to be: with
//! no `WF_CONTRACT_EXPECT` in the environment every test here panics saying so,
//! rather than quietly testing whichever `dl` the developer happens to have.
//! That distinction is the whole point — a contract test that silently accepts
//! the ambient install is the shim again, wearing a different hat.
//!
//! **Read-only.** The only two subcommands used are `--version` and
//! `--ls --json`, and both run under a scratch `HOME`, so this cannot see or
//! touch a real workspace. The scratch home matters for a second reason: a
//! listing read from the developer's own machine would assert against whatever
//! they happen to have cloned.

use std::path::{Path, PathBuf};
use std::process::Command;

use wf::launch::{Devlaunch, DEVLAUNCH_FLOOR, UNSAVED_IS_AN_OBJECT};
use wf::reap::{parse_workspaces, Unsaved};

/// How `pixi.toml` tells this file what environment it is in.
///
/// Three variables rather than one, because they are three independent facts
/// and a single "environment name" would make this file re-derive them from it
/// — which is the transcription problem again, one repository further in.
const EXPECT: &str = "WF_CONTRACT_EXPECT";
const VERSION: &str = "WF_CONTRACT_DL";
const UNSAVED: &str = "WF_CONTRACT_UNSAVED";

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
                "no `dl` on PATH, but {EXPECT} says there should be one \
                 ({}). Is this the `default` environment?",
                promised(EXPECT)
            )
        })
}

/// A `HOME` with nothing in it, for the duration of one call.
///
/// Not cleaned up on a panic, deliberately: a failing contract test is a thing
/// somebody is about to go and look at, and the listing `dl` wrote is evidence.
/// It is under the temp dir, keyed by pid, so it costs nothing to leave.
fn scratch_home(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wf-contract-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch home");
    dir
}

/// Run the `dl` under test and return its stdout, or panic with its stderr.
fn ask_dl(args: &[&str], what: &str) -> String {
    let program = dl();
    let out = Command::new(&program)
        .args(args)
        .env("HOME", scratch_home(what))
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

#[test]
fn the_dl_under_test_is_the_one_the_environment_installed() {
    // `pixi run` prepends its prefix to the *inherited* PATH, so a developer's
    // own `~/.pixi/bin/dl` is still on it, several entries later. If the
    // environment's own devlaunch were ever missing — a feature not listed on
    // the environment, a solve that dropped it — every other test in this file
    // would carry on happily against the host's install and report a verdict
    // about a version nobody selected. This is the assertion that makes the
    // pin mean something.
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
    let printed = ask_dl(&["--version"], "version");
    assert_ne!(
        Devlaunch::from_version_output(&printed),
        Devlaunch::Unreadable,
        "`wf` could not read a version out of what this `dl` prints: {printed:?}"
    );

    let expected = promised(EXPECT);
    let got = verdict();
    match expected.as_str() {
        "usable" => assert_eq!(
            got,
            Devlaunch::Usable,
            "this dl ({}) should clear the {DEVLAUNCH_FLOOR} floor",
            printed.trim()
        ),
        "too-old" => assert!(
            matches!(got, Devlaunch::TooOld(_)),
            "this dl ({}) is below the {DEVLAUNCH_FLOOR} floor and should read \
             as TooOld, not {got:?}",
            printed.trim()
        ),
        other => panic!("{EXPECT} says {other:?}, which this file does not know"),
    }
}

#[test]
fn the_pinned_version_and_the_installed_one_are_the_same_release() {
    // Only the exactly-pinned environments set this. `latest` deliberately does
    // not: what it installs is whatever `pixi.lock` resolved, and a number
    // written in `pixi.toml` beside it would be a second place to update and a
    // stale assertion the day somebody forgot.
    let Ok(pinned) = std::env::var(VERSION) else {
        return;
    };
    let printed = ask_dl(&["--version"], "version");
    let printed = printed.trim();
    assert!(
        printed.split_whitespace().any(|word| word == pinned),
        "pixi pinned devlaunch {pinned} but the `dl` on PATH says {printed:?}"
    );

    // And the floor environment's pin *is* the constant. Two files, one
    // release: `pixi.toml` naming 0.0.24 and `launch.rs` naming something else
    // would mean the floor is never actually run at.
    if promised(EXPECT) == "usable" && pinned == DEVLAUNCH_FLOOR.to_string() {
        assert_eq!(
            DEVLAUNCH_FLOOR, UNSAVED_IS_AN_OBJECT,
            "these two constants have parted company; the environments below \
             assume `WF_CONTRACT_UNSAVED=object` follows from clearing the floor"
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
    // banner, a wrapper object, or a line of prose.
    //
    // The rows themselves cannot be reached this way. Making one means a real
    // devpod workspace and a Docker daemon, which is devlaunch's own e2e
    // suite's job, not this file's; the row *shape* is checked below, from the
    // emitter rather than from an instance.
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
    //
    // Reaching into `devlaunch.workspace_state` is reaching past the CLI, and
    // that is the point rather than an oversight: the CLI cannot be made to
    // emit a `wouldLose` row without a real clone holding real uncommitted
    // work, and the three arms are exactly what is worth pinning. The coupling
    // is honest — an ImportError here says devlaunch moved the function that
    // defines this contract, which is a thing `wf` needs to be told.
    if promised(UNSAVED) != "object" {
        return;
    }
    let program = dl();
    let emit = r#"
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
"#;
    let python = python_of(&program);
    let out = Command::new(&python)
        .args(["-c", emit])
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", python.display()));
    assert!(
        out.status.success(),
        "devlaunch could not be asked what it writes for `unsaved` — it has \
         probably moved or renamed `workspace_state.unsaved_as_json`, which is \
         the function that defines this contract:\n{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let emitted = String::from_utf8_lossy(&out.stdout);
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
        // Through the real parser, in a real row, rather than through a
        // `serde_json::from_str` on the field alone: `unsaved` is read as part
        // of a `Workspace`, and a row is what `dl` actually emits.
        let row = format!(
            r#"[{{"id":"ws","devlaunch":true,"repo":"blooop/wayfinder",
                  "branch":"wayfinder/wayfinder-1","state":"Stopped",
                  "unsaved":{answer}}}]"#
        );
        let parsed = parse_workspaces(row.as_bytes())
            .unwrap_or_else(|e| panic!("`wf` could not read {answer}: {e:#}"));
        assert_eq!(
            parsed[0].unsaved.as_ref(),
            Some(&want),
            "devlaunch writes {answer} and `wf` read it as {:?}",
            parsed[0].unsaved
        );
    }
}

#[test]
fn a_dl_older_than_the_object_release_has_no_object_to_write() {
    // The version boundary from the other side. `UNSAVED_IS_AN_OBJECT` claims
    // 0.0.24 is where the one-key object arrived; the way to be sure is that
    // the release below it cannot produce one. Without this, the constant is a
    // date somebody remembered.
    if promised(UNSAVED) != "sentence" {
        return;
    }
    let program = dl();
    let python = python_of(&program);
    let out = Command::new(&python)
        .args([
            "-c",
            "from devlaunch.workspace_state import unsaved_as_json",
        ])
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", python.display()));
    assert!(
        !out.status.success(),
        "this devlaunch has `unsaved_as_json`, so it writes the object form — \
         but {UNSAVED_IS_AN_OBJECT} is where `wf` believes that started, and \
         `dl --version` says {}",
        ask_dl(&["--version"], "version").trim()
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
    let object = promised(UNSAVED) == "object";
    assert_eq!(
        verdict().answers_unsaved(),
        object,
        "this dl {} the object form, but `wf` {} it to answer `unsaved` for \
         every clone it made",
        if object { "writes" } else { "predates" },
        if object { "does not expect" } else { "expects" }
    );
}
