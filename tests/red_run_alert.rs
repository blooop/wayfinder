//! A red live or scheduled run files an issue (#190).
//!
//! The live tier is deliberately non-blocking, and the weekly devlaunch
//! contract re-solve is the only leg that can discover a new devlaunch
//! release — so both depend entirely on somebody *noticing* a red run. The
//! alert workflow is what does the noticing: it reconciles one tracking issue
//! per watched workflow, filing on red, updating rather than duplicating, and
//! closing on the next green of the same class.
//!
//! Offline by design, like its neighbours: half the seam is the workflow text
//! itself (what it subscribes to, what it may write, which runs it reacts to),
//! and the other half is the reconciliation script, driven here against a
//! recording `gh` stub — the same shim ethos `tests/live_devlaunch.rs` applies
//! to `devpod` — so the create-vs-comment-vs-close decisions are asserted at
//! the boundary they cross, with no network and no real tracker involved.

use std::fs;
use std::path::PathBuf;

/// A file in this repository, read by its path relative to the crate root.
fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} ships in this repo: {e}", path.display()))
}

/// A workflow with its full-line comments removed, as in
/// `tests/devcontainer_prebuild.rs` and for the same reason: this repo's
/// workflows discuss their own triggers at length, so a substring check
/// against the raw text would be reading the commentary.
fn workflow(rel: &str) -> String {
    repo_file(rel)
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A top-level block of a workflow file — `on:`, `permissions:`, `jobs:` —
/// from its key to the next line that starts in column zero.
fn top_level_block(workflow: &str, key: &str) -> String {
    let start = workflow.find(&format!("\n{key}:")).map_or_else(
        || {
            assert!(
                workflow.starts_with(&format!("{key}:")),
                "the workflow declares `{key}:`"
            );
            0
        },
        |index| index + 1,
    );
    let rest = &workflow[start..];
    let mut block = String::new();
    for (number, line) in rest.lines().enumerate() {
        let starts_new_block =
            number > 0 && !line.starts_with([' ', '\t']) && !line.trim().is_empty();
        if starts_new_block {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }
    block
}

/// The `name:` a workflow declares — the string `workflow_run` subscribes by.
fn declared_name(rel: &str) -> String {
    workflow(rel)
        .lines()
        .find_map(|line| line.strip_prefix("name:"))
        .unwrap_or_else(|| panic!("{rel} declares a `name:`"))
        .trim()
        .trim_matches('"')
        .to_string()
}

const ALERT: &str = ".github/workflows/red-run-alert.yml";

/// `workflow_run` subscribes by *display name*, not by file name — so the
/// subscription is a string that nothing at GitHub's end validates, and a
/// rename of either watched workflow would disconnect the alert without a
/// word. This derives the expected names from the watched files' own `name:`
/// fields, so a rename either moves both ends or fails here.
#[test]
fn the_alert_watches_both_nonblocking_legs_by_their_declared_names() {
    let triggers = top_level_block(&workflow(ALERT), "on");
    assert!(
        triggers.contains("workflow_run:"),
        "the alert is triggered by the watched runs completing, not by a \
         schedule of its own:\n{triggers}"
    );
    assert!(
        triggers.contains("completed"),
        "`completed` is the only type that carries a conclusion — `requested` \
         fires before there is anything to reconcile:\n{triggers}"
    );
    for watched in [
        ".github/workflows/live.yml",
        ".github/workflows/devlaunch-contract.yml",
    ] {
        let name = declared_name(watched);
        assert!(
            triggers.contains(&format!("\"{name}\"")),
            "the alert subscribes to {watched} by its declared name \
             `{name}`:\n{triggers}"
        );
    }
}

/// The narrow-job pattern the repo's other workflows already follow: the
/// workflow as a whole can only read, and the ability to write issues is
/// granted to the one job that reconciles the tracking issue. The distinction
/// is what a step added later in some unrelated job could do — file and close
/// issues on this repo, or not.
#[test]
fn only_the_reconciling_job_may_write_issues() {
    let alert = workflow(ALERT);
    let workflow_level = top_level_block(&alert, "permissions");
    assert!(
        workflow_level.contains("contents: read"),
        "the default for the whole workflow is read-only:\n{workflow_level}"
    );
    assert!(
        !workflow_level.contains("issues:"),
        "issue write is never a workflow-wide grant — every job added later \
         would inherit it silently:\n{workflow_level}"
    );
    assert_eq!(
        top_level_block(&alert, "jobs")
            .matches("issues: write")
            .count(),
        1,
        "exactly one job may write issues: the one that reconciles the \
         tracking issue"
    );
}

/// The subscription above is wider than the promise: `live.yml` can be
/// dispatched at any branch, and `devlaunch-contract.yml` completes on every
/// pull request. The ticket's claim is about the runs nothing else stands
/// behind — live runs on `main` (the merge-then-run leg plus a dispatch aimed
/// there), and the contract workflow's scheduled re-solve, the one leg that
/// can discover a new devlaunch release. A red PR run of the contract already
/// blocks its PR; filing an issue for it would page the repo about something
/// the merge gate is holding anyway.
#[test]
fn the_alert_reacts_only_to_the_runs_nothing_else_stands_behind() {
    let jobs = top_level_block(&workflow(ALERT), "jobs");
    assert!(
        jobs.contains("github.event.workflow_run.head_branch == 'main'"),
        "live runs count only on main — a dispatch aimed at a topic branch is \
         somebody's experiment, not the world moving:\n{jobs}"
    );
    assert!(
        jobs.contains("github.event.workflow_run.event == 'schedule'"),
        "contract runs count only from the weekly re-solve — every other leg \
         blocks a pull request and needs no summons:\n{jobs}"
    );
}

/// Every guard above reads a field of the *watched* run, and a fork pull
/// request gets to choose most of them. A fork's default branch is `main`, so
/// a fork that adds a `pull_request:` leg to `live.yml` produces a run named
/// `live` whose `head_branch` is `main` and whose conclusion the fork's own
/// code decides — and this repo is public with first-time-contributor approval
/// only, so a prior contributor needs nobody's click. The consequence is worse
/// than a spurious summons: a forged `success` reaches the reconciling job,
/// which holds `issues: write` on the *base* repo, and **closes** a genuinely
/// red summons. The one field a fork cannot forge is which repository the run
/// belongs to, so that is what the job is required to check — and requiring it
/// of both legs rather than only the live one, because a guard that has to be
/// re-derived per leg is a guard the next leg forgets.
#[test]
fn a_run_from_a_fork_can_neither_file_nor_withdraw_a_summons() {
    let jobs = top_level_block(&workflow(ALERT), "jobs");
    assert!(
        jobs.contains("github.event.workflow_run.head_repository.full_name == github.repository"),
        "only runs belonging to this repository are reconciled — every other \
         field of a watched run is a fork's to choose, and a forged green \
         would withdraw a real summons:\n{jobs}"
    );
}

/// The reconciliation script, run for real against a recording `gh`.
///
/// The stub answers `gh issue list` with whatever the caller says the tracker
/// holds (the text a real `gh --jq` would print: matching issue numbers, one
/// per line, or nothing) and records every call's full argv — so each test
/// states a tracker state and a run conclusion, and asserts the writes that
/// crossed the boundary. Everything the script needs arrives in its
/// environment, exactly as the workflow hands it.
fn reconcile(test: &str, workflow_name: &str, conclusion: &str, open_issues: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(test);
    fs::create_dir_all(&dir).expect("the test scratch directory is writable");
    let log = dir.join("gh.log");
    let _ = fs::remove_file(&log);

    let stub = dir.join("gh");
    fs::write(
        &stub,
        "#!/usr/bin/env bash\n\
         printf '%s\\n' \"$*\" >> \"$GH_LOG\"\n\
         case \"$1 $2\" in\n\
         \x20 'issue list') printf '%s' \"$GH_OPEN_ISSUES\" ;;\n\
         \x20 'issue create') echo 'https://github.invalid/issues/999' ;;\n\
         esac\n",
    )
    .expect("the gh stub is writable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
            .expect("the gh stub can be made executable");
    }

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT);
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").expect("a test process has a PATH")
    );
    let output = std::process::Command::new(&script)
        .env("PATH", path)
        .env("GH_LOG", &log)
        .env("GH_OPEN_ISSUES", open_issues)
        .env("WORKFLOW_NAME", workflow_name)
        .env("CONCLUSION", conclusion)
        .env("RUN_URL", "https://github.invalid/actions/runs/1")
        .env("REPO", "blooop/wayfinder")
        .output()
        .unwrap_or_else(|e| panic!("{SCRIPT} ships in this repo and runs: {e}"));
    assert!(
        output.status.success(),
        "the script reconciles without error:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    match fs::read_to_string(&log) {
        Ok(calls) => calls.lines().map(str::to_string).collect(),
        // No log file means the stub was never invoked: zero gh calls.
        Err(_) => Vec::new(),
    }
}

const SCRIPT: &str = ".github/scripts/red_run_alert.sh";

/// A red run of a watched workflow with nothing already open files the
/// tracking issue, titled for the workflow so each watched leg reconciles
/// against its own issue and nobody has to read the body to know which leg is
/// red.
#[test]
fn a_red_run_with_no_open_issue_files_one() {
    let calls = reconcile("red_files", "live", "failure", "");
    let creates: Vec<&String> = calls
        .iter()
        .filter(|call| call.starts_with("issue create"))
        .collect();
    assert_eq!(creates.len(), 1, "one red run, one filed issue: {calls:?}");
    assert!(
        creates[0].contains("Red run: live"),
        "the title names the workflow that went red: {}",
        creates[0]
    );
    assert!(
        creates[0].contains("https://github.invalid/actions/runs/1"),
        "the issue points at the run it summons somebody to: {}",
        creates[0]
    );
    assert!(
        !calls.iter().any(|call| call.starts_with("issue comment")),
        "nothing existed to update: {calls:?}"
    );
}

/// A leg that stays red across several runs keeps a single summons: the
/// existing issue gains a comment pointing at the newest red run, and no
/// second issue appears. This is the idempotence the ticket asks for — the
/// failure mode it forecloses is a weekly schedule papering the tracker with
/// one issue per red Monday.
#[test]
fn a_red_run_with_an_open_issue_updates_it_instead_of_duplicating() {
    let calls = reconcile("red_updates", "live", "failure", "42\n");
    assert!(
        !calls.iter().any(|call| call.starts_with("issue create")),
        "the open issue is the summons — no duplicate: {calls:?}"
    );
    let comments: Vec<&String> = calls
        .iter()
        .filter(|call| call.starts_with("issue comment 42"))
        .collect();
    assert_eq!(
        comments.len(),
        1,
        "the existing issue is pointed at the newest red run: {calls:?}"
    );
    assert!(
        comments[0].contains("https://github.invalid/actions/runs/1"),
        "the update carries the run it reports: {}",
        comments[0]
    );
}

/// The next green run of the same class closes the summons itself, pointing
/// at the run that cleared it — the other half of idempotence: the issue's
/// open/closed state tracks the leg's red/green state with no human in the
/// loop.
#[test]
fn a_green_run_closes_the_open_summons() {
    let calls = reconcile("green_closes", "live", "success", "42\n");
    let closes: Vec<&String> = calls
        .iter()
        .filter(|call| call.starts_with("issue close 42"))
        .collect();
    assert_eq!(closes.len(), 1, "the summons is withdrawn: {calls:?}");
    assert!(
        closes[0].contains("https://github.invalid/actions/runs/1"),
        "the close names the green run that cleared it: {}",
        closes[0]
    );
    assert!(
        !calls.iter().any(|call| call.starts_with("issue create")),
        "a green run never files anything: {calls:?}"
    );
}

/// A green run with nothing open is the steady state — every merge to `main`
/// lands here — and it writes nothing at all: no issue, no comment, no noise
/// in the tracker for the ordinary case of things working.
#[test]
fn a_green_run_with_nothing_open_writes_nothing() {
    let calls = reconcile("green_quiet", "live", "success", "");
    assert!(
        !calls.iter().any(|call| !call.starts_with("issue list")),
        "the steady state is silent — the only gh call is the lookup: {calls:?}"
    );
}

/// Red is not spelled one way. A red X on the Actions tab is `failure`,
/// `timed_out` *or* `startup_failure`, and both of the latter are reachable on
/// the very legs this alert watches: `live.yml` declares no `timeout-minutes`,
/// so the leg whose whole job is asserting wall-clock budgets overruns into
/// GitHub's own timeout rather than into a test assertion, and a workflow that
/// stops parsing concludes `startup_failure` before there is a step to fail. A
/// summons that answers only to the literal string `failure` leaves exactly
/// those two unnoticed — the failure this whole workflow exists to foreclose,
/// reintroduced one string comparison in.
#[test]
fn every_red_conclusion_summons_somebody() {
    for conclusion in ["failure", "timed_out", "startup_failure"] {
        let calls = reconcile(&format!("red_{conclusion}"), "live", conclusion, "");
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("issue create"))
                .count(),
            1,
            "`{conclusion}` renders as a red run and files a summons: {calls:?}"
        );
        let open = reconcile(
            &format!("red_open_{conclusion}"),
            "live",
            conclusion,
            "42\n",
        );
        assert_eq!(
            open.iter()
                .filter(|call| call.starts_with("issue comment 42"))
                .count(),
            1,
            "and it updates the standing summons rather than withdrawing it: \
             {open:?}"
        );
    }
}

/// A cancelled or skipped run is neither red nor green — the trigger's
/// `completed` type delivers both anyway — and neither may move anything:
/// filing on one would summon somebody to a run that was superseded, and
/// closing on one would withdraw a summons on no evidence at all. Both
/// directions are asserted for each, against the tracker state where the wrong
/// write is possible. These two exclusions are also what keeps the test above
/// honest: red is defined as "not green", so a conclusion not named here
/// becomes a summons.
#[test]
fn a_run_with_no_verdict_is_neither_red_nor_green() {
    for conclusion in ["cancelled", "skipped"] {
        let with_summons = reconcile(&format!("{conclusion}_open"), "live", conclusion, "42\n");
        assert!(
            !with_summons
                .iter()
                .any(|call| !call.starts_with("issue list")),
            "an open summons survives a {conclusion} run: {with_summons:?}"
        );
        let without = reconcile(&format!("{conclusion}_quiet"), "live", conclusion, "");
        assert!(
            !without.iter().any(|call| !call.starts_with("issue list")),
            "a {conclusion} run files nothing: {without:?}"
        );
    }
}

/// Each watched workflow reconciles against its own issue: the title embeds
/// the workflow's name, and the lookup matches that title exactly — not as a
/// substring, and not against the other leg's issue. A red contract re-solve
/// while the live summons is open must file a second issue, not comment on
/// the first.
#[test]
fn each_watched_workflow_has_a_summons_of_its_own() {
    let calls = reconcile("own_summons", "devlaunch contract", "failure", "");
    let lookup = calls
        .iter()
        .find(|call| call.starts_with("issue list"))
        .expect("the reconciliation starts from what the tracker holds");
    assert!(
        lookup.contains(r#"select(.title == "Red run: devlaunch contract")"#),
        "the lookup is an exact-title match for this workflow's own summons: \
         {lookup}"
    );
    let create = calls
        .iter()
        .find(|call| call.starts_with("issue create"))
        .expect("a red run with no matching summons files one");
    assert!(
        create.contains("Red run: devlaunch contract"),
        "the filed issue is titled for the leg that went red: {create}"
    );
}

/// The reconciliation is a search-then-act on one issue, so two of them
/// running at once is how "one open issue per workflow" becomes two — the
/// exact outcome the ticket names as the thing to avoid. A single fixed
/// concurrency group is what forecloses it, and nothing else in this file
/// notices if it goes: deleting the block leaves every behavioural test above
/// green, because they drive the script one call at a time and the race lives
/// above the script entirely.
#[test]
fn two_completions_cannot_reconcile_at_once() {
    let group = top_level_block(&workflow(ALERT), "concurrency");
    assert!(
        group.contains("group: red-run-alert") && !group.contains("${{"),
        "one fixed group, not keyed on the ref or the event — a group per run \
         serialises nothing:\n{group}"
    );
    assert!(
        group.contains("cancel-in-progress: false"),
        "the reconciliation already running is the one holding the tracker \
         mid-decision; cancelling it is what leaves a duplicate:\n{group}"
    );
}

/// The lookup's flag surface, which the tests above cannot see: the stub
/// answers `issue list` whatever follows, so the flags that decide *which*
/// issues can match are asserted here or nowhere. `--state open` is the one
/// that carries weight, and dropping it to `--state all` is a silent break
/// rather than a loud one — a *closed* summons matches the exact title just as
/// well, so a leg that re-reddens after a green comments on an issue nobody is
/// watching and never files a fresh one. Reopening is a human's call; from
/// this script's side a closed summons is not a summons.
#[test]
fn the_lookup_asks_only_for_a_summons_that_is_still_open() {
    let calls = reconcile("lookup_state", "live", "failure", "");
    let lookup = calls
        .iter()
        .find(|call| call.starts_with("issue list"))
        .expect("the reconciliation starts from what the tracker holds");
    assert!(
        lookup.contains("--state open"),
        "only an open issue can be the standing summons: {lookup}"
    );
}

/// The `NAME: value` pairs a job block hands a step. Uppercase-with-underscore
/// names only, which is unambiguous in a workflow file: every key YAML itself
/// owns is lowercase or kebab-case, so anything shouting is an environment
/// variable.
fn handed_env(jobs: &str) -> Vec<(&str, &str)> {
    jobs.lines()
        .filter_map(|line| {
            let (name, value) = line.trim().split_once(':')?;
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
                .then_some((name, value.trim()))
        })
        .collect()
}

/// Which run each variable describes — the half of the env contract that
/// comparing key *names* cannot see, and the half where being wrong is
/// invisible. In a `workflow_run` run the context holds *two* runs: the
/// watched one, under `github.event.workflow_run`, and the alert's own, under
/// the bare `github.*` keys. Every variable here has to name the first. The
/// mistake is one token wide and silent in both directions:
/// `${{ github.workflow }}` in place of the watched run's `name` is always
/// `red-run alert`, so both watched legs reconcile against a single summons
/// titled for the alert itself — which is exactly what
/// `each_watched_workflow_has_a_summons_of_its_own` says is foreclosed, and it
/// stays green because that test drives the script with a name the test picked
/// rather than one the job derived. `html_url` likewise: `url` is the REST
/// endpoint, so the summons would point a human at JSON.
///
/// Asserted as literals, as `tests/devcontainer_prebuild.rs` asserts the
/// published image tag in both of the files that must agree on it.
#[test]
fn every_variable_describes_the_watched_run_and_not_the_alerts_own() {
    let jobs = top_level_block(&workflow(ALERT), "jobs");
    let handed = handed_env(&jobs);
    for (name, expression) in [
        // `github.token`, not a PAT: the job-scoped `issues: write` above is
        // the whole permission story, and a secret would route around it.
        ("GH_TOKEN", "${{ github.token }}"),
        ("WORKFLOW_NAME", "${{ github.event.workflow_run.name }}"),
        ("CONCLUSION", "${{ github.event.workflow_run.conclusion }}"),
        ("RUN_URL", "${{ github.event.workflow_run.html_url }}"),
        // The one that is legitimately about the alert's own run: the tracker
        // written to is this repo, which is also where the alert lives.
        ("REPO", "${{ github.repository }}"),
    ] {
        let handed_value = handed
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value);
        assert_eq!(
            handed_value,
            Some(expression),
            "the job hands {name} the expression that names the watched run"
        );
    }
}

/// The two halves tested above only meet if the job really invokes the script
/// and their environment contract holds in both directions: a variable the
/// script reads but the job never sets is an empty title or a `set -u` abort
/// on the runner, and one the job sets but nothing reads is a claim the
/// script silently stopped honouring. `GH_TOKEN` is the one deliberate
/// asymmetry — the job hands it for `gh` itself to consume, so no line of the
/// script names it.
///
/// This is the *names*, and names alone; which run each one describes is
/// `every_variable_describes_the_watched_run_and_not_the_alerts_own` above,
/// because a set of key names is identical whether the values are right or
/// point at the alert's own run.
#[test]
fn the_job_hands_the_script_exactly_what_it_reads() {
    let jobs = top_level_block(&workflow(ALERT), "jobs");
    assert!(
        jobs.contains(SCRIPT),
        "the reconciling job runs the script the tests above drove:\n{jobs}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT))
            .expect("the script ships in this repo")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "the runner invokes the script directly, so the executable bit \
             has to survive the checkout: mode {mode:o}"
        );
    }

    let mut handed: Vec<&str> = handed_env(&jobs)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| *name != "GH_TOKEN")
        .collect();
    handed.sort_unstable();
    // Deduped like `read` below. A job with two steps could legitimately hand
    // the same variable twice, and without this the comparison fails on the
    // repeat rather than on anything either side got wrong.
    handed.dedup();

    let script = repo_file(SCRIPT);
    let mut read: Vec<&str> = script
        .match_indices("${")
        .filter_map(|(index, _)| {
            let rest = &script[index + 2..];
            let name = &rest[..rest.find('}')?];
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
                .then_some(name)
        })
        .collect();
    read.sort_unstable();
    read.dedup();

    assert_eq!(
        handed, read,
        "what the job hands (left) is exactly what the script reads (right), \
         GH_TOKEN aside"
    );
}
