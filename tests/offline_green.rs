//! The offline-green promise, held structurally (#165).
//!
//! A bare `cargo test` is green in a fresh checkout with no network, no
//! authenticated `gh` and no `devlaunch` (#42) — not because the tests that
//! need those are absent but because each carries `#[ignore]`. That promise
//! rests entirely on an attribute, and an attribute is the kind of thing a new
//! test forgets: nothing about writing one reminds anybody, and the omission is
//! invisible in both directions. Unignored, a live test breaks the bare run on
//! every machine lacking what it wants; and being unignored it is also never
//! selected by `live.yml`, which runs the gated set with `--ignored`. So the
//! rule was stated in prose in AGENTS.md and checked nowhere.
//!
//! It is checked here, in the `devcontainer_prebuild.rs` mould and offline for
//! the same reason: the seam is the source text. What is missing is an
//! attribute, which no run of the tests can notice — the failure is precisely
//! that they run when they should not, or never run at all.
//!
//! This binary is deliberately not named `live_*`. It is not a live test, and
//! under that glob the walk below would walk itself and demand its own gating,
//! which would gate the guard out of the run it exists to guard.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// One `tests/live_*.rs` source: its file name, and its text.
///
/// The set is read off the directory rather than listed, so a live binary
/// added tomorrow is covered by having been added — a hand-maintained list
/// would decay exactly the way the attribute does.
///
/// Top-level `tests/live_*.rs` files only, and that scope is load-bearing: the
/// tests compiled in through `mod common;` are deliberately unignored (they
/// are the offline fixture diagnostics), and a directory-style target
/// (`tests/live_x/main.rs`) would sit outside this walk — adding one means
/// teaching this function to descend, or the guard goes quietly blind to it.
fn live_sources() -> Vec<(String, String)> {
    let tests = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));
    let mut sources: Vec<(String, String)> = std::fs::read_dir(&tests)
        .expect("this repo's tests directory")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter_map(|path| {
            let is_rust = path.extension().is_some_and(|extension| extension == "rs");
            let name = path.file_name()?.to_string_lossy().into_owned();
            (is_rust && name.starts_with("live_")).then(|| {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{name} is readable: {e}"));
                (name, text)
            })
        })
        .collect();
    sources.sort();
    sources
}

/// A test function, with the run of attributes written above it.
struct TestFn {
    name: String,
    line: usize,
    attributes: Vec<String>,
}

/// Whether an attribute line is one that makes the item below it a test:
/// `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = "multi_thread")]`.
/// Matched on the attribute's path rather than the whole line, so arguments and
/// the choice of runtime are free to change.
fn is_test_attribute(attribute: &str) -> bool {
    let path = attribute
        .trim_start_matches("#[")
        .split(['(', ']'])
        .next()
        .unwrap_or_default()
        .trim();
    path == "test" || path.ends_with("::test")
}

/// Whether an attribute line gates the item below it out of a default run.
/// Both spellings count — bare `#[ignore]` and `#[ignore = "why"]` — because
/// what this file guards is the gating; the reason string is a courtesy this
/// repo pays everywhere and no test needs to be told to.
fn is_ignore_attribute(attribute: &str) -> bool {
    let path = attribute
        .trim_start_matches("#[")
        .split(['(', '=', ']'])
        .next()
        .unwrap_or_default()
        .trim();
    path == "ignore"
}

/// The name a function signature declares, if the line is one.
fn declared_function(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    // Qualifiers a test function may carry between the attributes and `fn`.
    while let Some(shorter) = ["pub(crate) ", "pub ", "async ", "unsafe ", "const "]
        .iter()
        .find_map(|qualifier| rest.strip_prefix(qualifier))
    {
        rest = shorter.trim_start();
    }
    let named = rest.strip_prefix("fn ")?.trim_start();
    let end = named
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(named.len());
    (end > 0).then(|| named[..end].to_string())
}

/// Every test function in one source, with its attributes.
///
/// Line-based on purpose. The alternative is a real Rust parser — `syn` and a
/// proc-macro toolchain — to read an attribute this repo writes one way, in a
/// file rustfmt has already normalised; that is a dependency and a build cost
/// out of all proportion to the contract. What it costs instead is a scanner
/// with limits, so the limits are named and then checked rather than assumed:
///
/// - Attributes are recognised a line at a time. One split across lines is not
///   seen as an attribute, and clears the run it was part of.
/// - `#[test] fn thing()` written on a single line is not seen as a function.
/// - A test generated by a macro has no attribute here to find.
/// - Attribute-shaped text inside a multi-line string literal is scanned as if
///   it were real: a string carrying `#[ignore = "…"]` directly above a test
///   would vouch for it, and the count check below cannot catch that
///   direction. No live file embeds such a snippet; one that did would be
///   quoting this guard's own subject matter, which is a review problem
///   before it is a scanner problem.
/// - `#[cfg_attr(…, ignore)]` is reported as ungated — loudly, with advice
///   naming the plain form. This repo gates unconditionally, on purpose.
///
/// The first two would silently drop a test from the walk, so
/// `every_test_attribute_in_a_live_source_is_one_this_guard_can_see` compares
/// what was collected against the test attributes the file contains and fails
/// loudly on the difference. The third is beyond any textual guard, and this
/// repo writes no such macro.
fn tests_in(source: &str) -> Vec<TestFn> {
    let mut found = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("#[") {
            attributes.push(line.to_string());
        } else if line.is_empty() || line.starts_with("//") {
            // Doc comments, ordinary comments and blank lines sit between
            // attributes freely and say nothing about gating.
        } else {
            if let Some(name) = declared_function(line) {
                if attributes.iter().any(|a| is_test_attribute(a)) {
                    found.push(TestFn {
                        name,
                        line: index + 1,
                        attributes: attributes.clone(),
                    });
                }
            }
            attributes.clear();
        }
    }
    found
}

/// Every test in `tests/live_*.rs` carries `#[ignore]`.
///
/// In either order and with or without a reason string: what is asserted is
/// that the attribute is on the function, not where the author put it.
#[test]
fn every_test_in_a_live_source_is_gated_behind_ignore() {
    let sources = live_sources();
    assert!(
        !sources.is_empty(),
        "no `tests/live_*.rs` sources were found, so this guard just passed \
         without looking at anything. Either the live tests moved or were \
         renamed out of that pattern — follow them, or this file is a green \
         light wired to nothing"
    );

    let mut tests_seen = 0;
    let mut ungated = Vec::new();
    for (name, text) in &sources {
        for test in tests_in(text) {
            tests_seen += 1;
            if !test.attributes.iter().any(|a| is_ignore_attribute(a)) {
                ungated.push(format!("{name}:{} {}", test.line, test.name));
            }
        }
    }

    assert!(
        tests_seen > 0,
        "the live sources exist but no tests were found in them, which means \
         the scanner below stopped recognising how they are written rather \
         than that the tests are gone"
    );
    assert!(
        ungated.is_empty(),
        "these live tests run in a default `cargo test`, and a live test that \
         runs by default fails on any machine without network, an \
         authenticated `gh` or the chosen `devlaunch` — the bare run is \
         supposed to be green in a fresh checkout (#42). Missing the \
         attribute, they are also never selected by `live.yml`, which runs the \
         gated set with `--ignored`: unobserved in both directions. Add \
         `#[ignore = \"live: <what it needs>\"]`, and a line enrolling the \
         binary in the workflow that can supply it.\n  {}",
        ungated.join("\n  ")
    );
}

/// The scanner above saw every test in the file, not merely every test it
/// happened to parse.
///
/// A guard that reads source by line can be walked past by a shape it does not
/// recognise, and the failure mode would be the quiet one: a test dropped from
/// the walk is a test reported as gated. So the two counts are compared, and a
/// shape this file cannot read becomes a red run asking for the scanner to be
/// taught it — the loud failure, which is the only kind worth having here.
#[test]
fn every_test_attribute_in_a_live_source_is_one_this_guard_can_see() {
    for (name, text) in &live_sources() {
        let written = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("#[") && is_test_attribute(line))
            .count();
        let paired = tests_in(text).len();
        assert_eq!(
            paired, written,
            "{name} contains {written} test attributes but the scanner in this \
             file paired only {paired} of them with a function. The difference \
             is written in a shape it cannot read — an attribute split across \
             lines, or an attribute and its `fn` on one line — and a test it \
             cannot read is a test it cannot report as ungated. Teach it the \
             shape, or write the test the way the rest of the file does"
        );
    }
}

/// One file of this repository, by its path relative to the crate root.
fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{relative} ships in this repo: {e}"))
}

/// Every test binary cargo builds out of `tests/`, by the name `--test` selects
/// it with.
///
/// Read off the directory for the same reason `live_sources` is, and both forms
/// cargo auto-discovers rather than only the common one: `tests/<name>.rs`, and
/// `tests/<name>/main.rs`. The directory form is the one the walk above admits
/// it goes blind to, and blindness is the whole hazard here — a target no walk
/// sees is a target this guard silently vouches for. A directory with no
/// `main.rs` is not a target at all (`tests/common/` is a module the live
/// binaries include), so it is not one this guard demands a workflow line for.
fn test_targets() -> Vec<String> {
    let tests = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"));
    let mut targets: Vec<String> = std::fs::read_dir(&tests)
        .expect("this repo's tests directory")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            if path.is_dir() {
                path.join("main.rs").is_file().then_some(name)
            } else {
                path.extension()
                    .is_some_and(|extension| extension == "rs")
                    .then_some(name)
            }
        })
        .collect();
    targets.sort();
    targets
}

/// Where a test binary can be enrolled: a file that names binaries on a
/// `cargo test` command line, and what makes the commands in it run.
///
/// The third entry is a different kind of thing from the first two, and saying
/// so is the point of pairing each path with a reason. `ci.yml` and `live.yml`
/// carry their own triggers, so a `--test` line in either is a line GitHub
/// executes. `pixi.toml` carries no trigger whatsoever: its tasks run because
/// `devlaunch-contract.yml` calls them by name, one indirection away. That
/// makes this list a claim rather than an observation, which is why
/// `the_pixi_manifest_is_a_runner_only_because_a_workflow_reaches_it` re-checks
/// the call instead of trusting the row.
const RUNNERS: &[(&str, &str)] = &[
    (
        ".github/workflows/ci.yml",
        "one `cargo test --test <name>` step, beside the offline binaries already there",
    ),
    (
        ".github/workflows/live.yml",
        "a `--test <name>` line in the gated set it selects with `--ignored`",
    ),
    (
        "pixi.toml",
        "a task `devlaunch-contract.yml` calls, for anything wanting a chosen `dl`",
    ),
];

/// The test binaries one runner's text names.
///
/// Comment lines come out first, and that is load-bearing rather than tidy:
/// this repo's workflows and its pixi manifest are mostly prose, and the prose
/// discusses the very binaries being counted — `ci.yml` explains at length why
/// `live.yml` names its binaries one by one. Counted raw, a comment would
/// enrol a binary nothing runs, which is precisely the failure this guard
/// exists to catch, arriving through the guard itself.
///
/// The trailing space in the pattern is doing real work too: `--test-threads=1`
/// appears in both files that run gated tests and names no binary.
fn binaries_named_in(text: &str) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    for line in text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        let mut rest = line;
        while let Some(index) = rest.find("--test ") {
            rest = &rest[index + "--test ".len()..];
            let end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            if end > 0 {
                named.insert(rest[..end].to_string());
            }
        }
    }
    named
}

/// Every enrolled test binary, and which runners name it.
fn enrolment() -> BTreeMap<String, Vec<&'static str>> {
    let mut enrolled: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for (path, _) in RUNNERS {
        for binary in binaries_named_in(&repo_file(path)) {
            enrolled.entry(binary).or_default().push(path);
        }
    }
    enrolled
}

/// Every test binary under `tests/` is named by a command something runs.
///
/// The sibling above guards the attribute half of enrolment; this guards the
/// half AGENTS.md used to hand back to a human. A `tests/foo.rs` with no
/// `live_` prefix compiles under `ci.yml`'s `--no-run` step, carries no
/// `#[ignore]` for the walk above to want, and is selected by no workflow: it
/// is run by nothing and flagged by nothing. That is the "an assertion nothing
/// runs is not an assertion" hole this repo closes everywhere else, reopened
/// one meta-level up — a whole binary of assertions, rather than one test,
/// quietly not running.
#[test]
fn every_test_binary_is_named_by_a_workflow_that_runs_it() {
    let targets = test_targets();
    let enrolled = enrolment();
    assert!(
        !targets.is_empty(),
        "no test binaries were found under `tests/`, so this guard just passed \
         without looking at anything"
    );
    assert!(
        !enrolled.is_empty(),
        "no runner names a single test binary, which means the scan below \
         stopped recognising how they are enrolled rather than that enrolment \
         is gone: {RUNNERS:?}"
    );

    let orphans: Vec<String> = targets
        .iter()
        .filter(|target| !enrolled.contains_key(*target))
        .map(|target| format!("tests/{target}"))
        .collect();
    assert!(
        orphans.is_empty(),
        "these test binaries are run by nothing. They compile — `ci.yml`'s \
         `--all-targets --no-run` step sees to that — so rot is caught, but \
         every assertion inside them is dead weight nobody is told about, \
         which is worse than not having written them. Enrol each one, in \
         whichever of these fits what it needs:\n  {}\nOrphans:\n  {}",
        RUNNERS
            .iter()
            .map(|(path, how)| format!("{path}: {how}"))
            .collect::<Vec<_>>()
            .join("\n  "),
        orphans.join("\n  ")
    );
}

/// Nothing is enrolled that does not exist.
///
/// The other direction of the same contract, and the one a rename leaves
/// behind: `--test gone_away` makes `cargo test` fail loudly, so this would be
/// caught — but only by the workflow holding the stale line, and `live.yml`
/// runs after a merge while `pixi.toml`'s tasks run in a workflow whose red
/// runs already mean "the other repo moved". Caught here, it is caught at the
/// commit that renamed the file, in the same run as the forward sweep, and by
/// the same reading of the same two facts.
#[test]
fn no_runner_names_a_test_binary_that_does_not_exist() {
    let targets: BTreeSet<String> = test_targets().into_iter().collect();
    let stale: Vec<String> = enrolment()
        .iter()
        .filter(|(binary, _)| !targets.contains(*binary))
        .map(|(binary, runners)| format!("{binary} (named in {})", runners.join(", ")))
        .collect();
    assert!(
        stale.is_empty(),
        "these `--test` arguments name no test binary in this repo, so the \
         command carrying them cannot run at all — a rename that moved the \
         file and left the enrolment, most likely. Point each at the binary \
         it meant, or drop the argument:\n  {}",
        stale.join("\n  ")
    );
}

/// The scan above reads commands, not the prose around them.
///
/// The self-check the two sweeps rest on, and they rest on it heavily: both
/// ask whether a name appears, so anything that over-reports enrolment turns
/// the forward sweep into a green light wired to nothing — the failure it was
/// written to prevent, wearing its own badge. Two limits of the scan are
/// pinned here rather than trusted, on text written in this test so the check
/// keeps meaning what it says however the real files are edited: a comment
/// enrols nothing, and `--test-threads` is not a binary called `threads`. The
/// third assertion is the direct one — a name no file anywhere writes is not
/// enrolled — because a scan that reported everything as enrolled would pass
/// both sweeps in silence.
#[test]
fn the_scan_reads_commands_rather_than_the_prose_around_them() {
    assert!(
        binaries_named_in("      # run: cargo test --locked --test discussed_only\n").is_empty(),
        "a commented-out or discussed command enrols nothing. The workflows in \
         this repo are mostly prose and the prose names test binaries; if a \
         comment counts, the forward sweep above can be satisfied by a \
         sentence rather than by a step"
    );
    assert_eq!(
        binaries_named_in("run: cargo test --test real_one -- --ignored --test-threads=1"),
        BTreeSet::from([String::from("real_one")]),
        "`--test-threads` names no binary, and both files that run gated tests \
         pass it"
    );
    assert!(
        !enrolment().contains_key("wayfinder_189_no_runner_names_this"),
        "a binary no file in this repo mentions came back enrolled, so the scan \
         is reporting enrolment it cannot have read and both sweeps above are \
         green for no reason"
    );
}

/// Every workflow in this repository, as `(file name, contents)`.
///
/// Read from the directory rather than from a list, on the same principle as
/// every other walk in this file: a workflow added later is searched by having
/// been added.
fn workflows() -> Vec<(String, String)> {
    let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.github/workflows"));
    let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("this repo's workflow directory")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "yml"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("a path read from a directory has a file name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{name} is readable: {e}"));
            (name, text)
        })
        .collect();
    found.sort();
    found
}

/// The pixi task one command line calls, if it is a `pixi run`.
///
/// `-e <environment>` is skipped rather than counted: the environment decides
/// which `dl` the task meets, not which task runs, and the contract workflow
/// calls the same task in four of them.
fn pixi_task_called_on(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace().skip_while(|token| *token != "pixi");
    tokens.next()?;
    if tokens.next()? != "run" {
        return None;
    }
    let mut next = tokens.next()?;
    if next == "-e" {
        tokens.next()?;
        next = tokens.next()?;
    }
    Some(next.to_string())
}

/// The pixi manifest counts as a runner only because a workflow calls it.
///
/// `RUNNERS` lists three files, and this one is the odd one out: the two
/// workflows carry their own triggers, so a `--test` line in either is a line
/// GitHub runs, while `pixi.toml` has no trigger at all. Its `--test` lines run
/// only because `devlaunch-contract.yml` names the task holding them. Drop that
/// call and the manifest becomes a file full of commands nothing invokes — at
/// which point the forward sweep above would go on reporting the binaries it
/// names as enrolled, which is exactly the lie this whole file exists to catch.
/// So the delegation is asserted, not assumed.
#[test]
fn the_pixi_manifest_is_a_runner_only_because_a_workflow_reaches_it() {
    let tasks_that_run_tests: BTreeSet<String> = repo_file("pixi.toml")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && line.contains("--test "))
        .filter_map(|line| Some(line.split('=').next()?.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect();
    assert!(
        !tasks_that_run_tests.is_empty(),
        "no pixi task names a test binary any more, so `pixi.toml` has no \
         business in `RUNNERS` — either the contract run moved somewhere else, \
         or the way tasks are written changed under the read above"
    );

    let called: BTreeSet<String> = workflows()
        .iter()
        .flat_map(|(_, text)| {
            text.lines()
                .filter(|line| !line.trim_start().starts_with('#'))
                .filter_map(pixi_task_called_on)
                .collect::<Vec<_>>()
        })
        .collect();
    let unreached: Vec<&String> = tasks_that_run_tests
        .iter()
        .filter(|task| !called.contains(*task))
        .collect();
    assert!(
        unreached.is_empty(),
        "these pixi tasks run test binaries and no workflow calls them, so \
         every binary they name is enrolled on paper and run nowhere — and the \
         sweep above cannot tell, because it reads the manifest and trusts that \
         something reaches it. Call the task from a workflow, or stop counting \
         `pixi.toml` as a runner.\n  unreached: {unreached:?}\n  tasks a \
         workflow calls: {called:?}"
    );
}

/// A test binary's name is the one cargo derives from its path.
///
/// The walk above turns `tests/foo.rs` into the name `foo` and then looks for
/// `--test foo`. That equivalence is auto-discovery's, not a rule of the
/// language: a `[[test]]` table in the manifest can call any file anything, at
/// which point a file this guard demands enrolment for is enrolled under a name
/// it does not know, and a name the workflows use belongs to no file it can
/// find. It would report both directions wrong at once. This repo declares no
/// test targets by hand — there is nothing to configure that a file name does
/// not already say — so the derivation is kept sound by keeping that true.
#[test]
fn a_test_binarys_name_is_the_one_cargo_derives_from_its_path() {
    let manifest = repo_file("Cargo.toml");
    let declared = manifest
        .lines()
        .map(str::trim)
        .filter(|line| *line == "[[test]]")
        .count();
    assert!(
        declared == 0,
        "Cargo.toml declares {declared} test target(s) by hand. The walk in this file \
         reads names off `tests/` and compares them to `--test` arguments, and \
         a declared target parts those two: it can name `tests/a.rs` `b`, so \
         `a` is demanded and never enrolled while `b` is enrolled and matches \
         no file. Either drop the table and let auto-discovery name the target, \
         or teach the walk to read the manifest first"
    );
}

/// No live binary hides from the ignore walk in the directory form.
///
/// `live_sources` reads `tests/live_*.rs` and says in as many words that a
/// `tests/live_x/main.rs` would sit outside it. That admission was the only
/// thing standing between this repo and a live binary whose tests are
/// unchecked for `#[ignore]` — and unchecked in the quiet direction, since the
/// walk would report the file it cannot see as compliant. Now that the walk
/// above enumerates the directory form, the admission can be enforced instead
/// of merely written down: a live target in that shape fails here, naming the
/// two choices.
#[test]
fn no_live_binary_hides_from_the_ignore_walk_in_the_directory_form() {
    let scanned: BTreeSet<String> = live_sources()
        .into_iter()
        .map(|(name, _)| name.trim_end_matches(".rs").to_string())
        .collect();
    let unscanned: Vec<String> = test_targets()
        .into_iter()
        .filter(|target| target.starts_with("live_") && !scanned.contains(target))
        .collect();
    assert!(
        unscanned.is_empty(),
        "these live test binaries are directory-style targets, so the walk that \
         checks every live test carries `#[ignore]` never opens them — and a \
         file it never opens is a file it reports as gated. Flatten each to \
         `tests/<name>.rs`, or teach `live_sources` to descend into \
         `tests/<name>/`:\n  {}",
        unscanned.join("\n  ")
    );
}
