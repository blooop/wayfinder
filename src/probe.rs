//! Test scaffolding shared by more than one module.
//!
//! Two things live here, both compiled into the library's test build **and**
//! the binary's (`#[cfg(test)] mod probe;` in `lib.rs` and in `main.rs`), which
//! is the only way a piece of test support can be reached from both crates.
//!
//! [`record`] is the important one. `wf`'s dangerous edges are all subprocess
//! calls — `dl` and `gh` — and a subprocess call is *observable*: put a
//! recording shim first on `PATH` and every argv the code under test reached
//! for is written down. That is weaker than "this path cannot delete anything"
//! and stronger than a grep: it covers the branches the probe actually drives,
//! for as long as it watches, and within that it does not care how the deletion
//! was spelt. A mutation that names none of the forbidden tokens still has to
//! run `dl` to destroy a workspace, and running `dl` is exactly what this sees.
//!
//! The shims have to be first on the `PATH` of the process doing the work, and
//! `PATH` is per-process, so the work happens in a **child**: an `#[ignore]`d
//! test in this same binary, named by `--exact` and run with `--ignored`. That
//! keeps the environment surgery out of the parent — where it would race every
//! other test in the binary — and costs one process spawn.
//!
//! The child also gets a scratch `HOME` laid out as a machine with the
//! fixture's workspaces on it, and the whole tree is compared before and
//! after. That is the other half of the same idea, for the destruction that
//! runs no command: `std::fs::remove_dir_all` leaves no argv, but it does
//! leave a hole.
//!
//! [`note`] is what makes the log a timeline the probe body can put its own
//! marks on, so an *ordering* — this frame was painted before that subprocess
//! ran — is as observable as the argv itself. Notes are prefixed so that
//! [`Recording::argv`] holds subprocesses and nothing else: a note is written
//! by the probe, not by the code under test, and one that happened to contain
//! the word `rm` would otherwise fail a deletion assertion.
//!
//! [`code_only`] is the smaller one: this crate's own source with the comments
//! and the test module stripped, for the structural guards in
//! [`reclaim`](crate::reclaim) and [`refresh`](crate::refresh). It was written
//! twice, byte for byte, before it was written here.

// This file is compiled into two crates and neither uses all of it — the
// library's tests want `code_only`, the binary's do not. Without this, whatever
// one crate does not reach is a `dead_code` warning, and CI makes a warning a
// failure.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The env var carrying the recording log's path down to the shims. Set on the
/// child by [`record`]; the shims inherit it through the child's own
/// `Command::new("dl")`, which is the whole reason it is an env var and not an
/// argument.
const LOG: &str = "WF_PROBE_LOG";

/// What a probe body puts in front of anything it wants read back.
///
/// The child's stdout is shared with the test harness's own chatter, and
/// `test tests::reclaimable_arm_probe ... ` lands on the same line as the first
/// thing printed — a line that contains the word "reclaimable" purely because
/// the test is named for it. Everything the probe says is marked, and
/// [`Recording::printed`] keeps only the marked part.
pub const MARK: &str = "PROBE|";

/// What [`note`] puts in front of its lines, so the one log can be read back
/// two ways.
///
/// The shims and the probe body append to the same file, which is what makes
/// an *ordering* observable — but they are not the same kind of fact. A shim
/// line is something the code under test did; a note is something the test
/// said about it. Without a marker the two are one list, and a probe that
/// noted `<rm>` would fail [`Recording::destroyed_nothing`] on its own
/// commentary. A shim writes a program name first, and a program name cannot
/// contain a `|`.
const NOTED: &str = "note|";

/// The workspace ids [`record`] lays a scratch home out for — the four
/// [`DL_LISTING`] names, so every workspace the code under test can *see* is
/// also a directory it can be caught destroying.
const LAID_OUT: [&str; 4] = [
    "wf-129-closed",
    "wf-138-unstarted",
    "wf-137-open",
    "wf-134-stalled",
];

/// The repo those four belong to in [`DL_LISTING`], which is also the
/// directory `dl` files their clones under.
const LAID_OUT_REPO: &str = "blooop/wayfinder";

/// Where `dl` keeps the clone of each workspace, relative to `HOME`, and where
/// it keeps the registry naming them all.
///
/// Read off a real machine rather than invented: `dl` clones a bare repo to
/// `~/.cache/devlaunch/repos/<owner>/<repo>/.bare` and adds one worktree
/// directory per workspace beside it, with `~/.cache/devlaunch/metadata.json`
/// recording where each one went.
const CLONES: &str = ".cache/devlaunch/repos";
const REGISTRY: &str = ".cache/devlaunch/metadata.json";

/// Where devpod keeps its own record of each workspace, relative to `HOME`.
/// `default` is the context name on a machine nobody has configured a second
/// one on, which is every machine `wf` has run on.
const RECORDS: &str = ".devpod/contexts/default/workspaces";

/// What each clone holds. The name is the point: this stands in for the one
/// thing `dl`'s own guard exists to protect — a checkout holding work that
/// exists nowhere else.
const PRECIOUS: &str = "work that exists nowhere else\n";

/// What a probe run saw.
#[derive(Debug)]
pub struct Recording {
    /// One line per subprocess the child ran, in order: the program name and
    /// then each argument in angle brackets, so an argument containing a space
    /// cannot be mistaken for two.
    ///
    /// Subprocesses only. The probe body's own [`note`]s share the log, so
    /// that the two can be *ordered* against each other, but they are not
    /// things the code under test ran and they are not counted here — see
    /// [`Recording::timeline`] for the merged view.
    pub argv: Vec<String>,
    /// The same log with the notes left in and in order, for the assertions
    /// that are about *when* something happened rather than whether it did.
    pub timeline: Vec<String>,
    /// Everything the child printed.
    pub stdout: String,
    /// Every path under the child's scratch `HOME` that was not the same
    /// afterwards as before — removed, added or rewritten. Empty is the
    /// expected reading; see [`Recording::destroyed_nothing`].
    disturbed: Vec<String>,
}

impl Recording {
    /// What the probe body itself printed, one entry per [`MARK`]ed write.
    pub fn printed(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .filter_map(|line| line.split_once(MARK).map(|(_, said)| said))
            .collect()
    }

    /// Fail unless the run reached for no external command at all.
    ///
    /// The assertion for the paths that are supposed to be pure — folding a
    /// value into the app state, drawing a screen — where *any* argv is the
    /// defect, whatever it says.
    pub fn ran_nothing(&self) {
        assert!(
            self.argv.is_empty(),
            "this path must run no external command at all, and it ran: {:?}",
            self.argv
        );
    }

    /// Fail if the run destroyed anything, by either of the two means it has.
    ///
    /// **Out of process.** Deliberately not a list of function names — the
    /// point of watching argv rather than source text is that it does not
    /// matter *how* the deletion was spelt in Rust. `dl <ws> rm` is the only
    /// thing that removes a workspace, `--force` the only waiver, and both are
    /// visible here however they were reached.
    ///
    /// **In process.** A `std::fs::remove_dir_all` runs no command and leaves
    /// no argv, so the child is given a scratch `HOME` laid out the way a real
    /// machine keeps its workspaces — see [`lay_out_a_home`] for the paths and
    /// where they were read from — and the whole tree is compared before and
    /// after. Any path under it that was removed, added or rewritten fails
    /// here, whatever module or alias or submodule did it.
    ///
    /// What that does **not** reach is a path outside the scratch home: a
    /// `remove_dir_all("/etc")` is caught by nothing here, as it would be in
    /// any code in any file. Nor does it reach a deletion the run never gets
    /// to — the recording ends when the probe body does, so a cleanup deferred
    /// past that is invisible here. Both limits are real and neither is
    /// closable by a test; what this covers is a destruction aimed at the
    /// workspaces the reading names, during the run.
    pub fn destroyed_nothing(&self) {
        for line in &self.argv {
            for forbidden in ["<rm>", "<--force>", "<remove>", "<delete>"] {
                assert!(
                    !line.contains(forbidden),
                    "this path must not be able to destroy a workspace, and it ran: {line}"
                );
            }
        }
        assert!(
            self.disturbed.is_empty(),
            "this path must leave the machine as it found it, and it changed: {:?}",
            self.disturbed
        );
    }
}

/// Run one `#[ignore]`d test of this binary in a child process whose `dl` and
/// `gh` are recording shims, and report what it ran and what it printed.
///
/// `dl_stdout` and `gh_stdout` are what those shims print — the fixture
/// listing and the fixture tracker answer. Both shims exit 0, so a path that
/// treats a failure as "no hint" cannot pass by accident.
///
/// # Panics
///
/// If the child could not be run, or failed. A failed child is a failed probe:
/// its assertions are the test's.
pub fn record(test: &str, dl_stdout: &str, gh_stdout: &str) -> Recording {
    record_as_dl(test, dl_stdout, gh_stdout, SHIMMED_DL)
}

/// [`record`], with the release the shimmed `dl` claims to be.
///
/// Split out for the one question [`record`] cannot ask: what `wf` does
/// **differently** either side of [`DEVLAUNCH_FLOOR`]. That is a rule about two
/// versions, so a harness that can only produce one cannot pin it — it can
/// watch the launch a new `dl` gets and call the conditional tested, which is
/// how the first version of this guard came to pass against a `wf` with the
/// floor check deleted.
///
/// The caller owns the pairing, and it is a real constraint rather than a
/// formality: `dl_stdout` has to be a listing the named release could have
/// written, or the probe is evidence about a machine that does not exist. See
/// [`SHIMMED_DL`], which is what to pass whenever the version is not itself
/// the subject.
///
/// [`DEVLAUNCH_FLOOR`]: crate::launch
pub fn record_as_dl(test: &str, dl_stdout: &str, gh_stdout: &str, dl_version: &str) -> Recording {
    let dir = scratch(test);
    std::fs::write(dir.join("dl.out"), dl_stdout).expect("the dl fixture");
    std::fs::write(dir.join("gh.out"), gh_stdout).expect("the gh fixture");
    let log = dir.join("argv.log");
    std::fs::write(&log, "").expect("the log");
    shim(&dir, "dl", dl_version);
    shim(&dir, "gh", dl_version);
    let home = lay_out_a_home(&dir);
    let before = tree(&home);

    let exe = std::env::current_exe().expect("this test binary");
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(exe)
        .args([
            "--exact",
            test,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PATH", path)
        .env("HOME", &home)
        .env(LOG, &log)
        .output()
        .expect("the probe child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let timeline: Vec<String> = std::fs::read_to_string(&log)
        .expect("the log")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let argv: Vec<String> = timeline
        .iter()
        .filter(|line| !line.starts_with(NOTED))
        .cloned()
        .collect();
    let disturbed = differences(&before, &tree(&home));
    // Read the log and the home *before* asserting anything, so that the
    // scratch directory is swept on the way out of a failing run too — a probe
    // that leaked a directory per failure would fill `/tmp` on exactly the day
    // someone is running it in a loop.
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the probe child `{test}` failed:\n{stdout}\n{stderr}"
    );
    // A name that matches nothing leaves a child that ran no tests, exited 0
    // and recorded no argv — and every assertion about what the probe did *not*
    // do would then pass vacuously. This is what makes those assertions mean
    // something: the probe ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the probe child `{test}` ran no test — is that name still right?\n{stdout}"
    );
    Recording {
        argv,
        timeline,
        stdout,
        disturbed,
    }
}

/// A scratch `HOME` for the child: a machine with the fixture's workspaces on
/// it, at the paths a real machine keeps them, each holding a file worth
/// keeping.
///
/// The layout is copied from a real machine and not invented, and that is the
/// whole of why this is worth doing. An earlier version of this laid the
/// workspaces out at `$HOME/workspaces/<id>` — a path that exists nowhere, so
/// the tree comparison was watching a directory no deletion would ever be
/// aimed at, and a `remove_dir_all` pointed at the *real* clone passed it. What
/// a cleanup on this path would actually reach for is here instead:
///
/// - `~/.cache/devlaunch/repos/<owner>/<repo>/<id>` — the worktree `dl` clones,
///   which is where uncommitted work lives and the thing `dl`'s own unsaved-work
///   guard exists to protect;
/// - `~/.cache/devlaunch/metadata.json` — the registry that says where each of
///   those went, which a cleanup rewrites rather than removes;
/// - `~/.devpod/contexts/default/workspaces/<id>` — devpod's record of the
///   container, which is what `dl <id> rm` deletes when there is no clone.
///
/// [`tree`] compares contents as well as names, so the rewritten registry is as
/// visible as the removed clone.
fn lay_out_a_home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let mut worktrees = Vec::new();
    for id in LAID_OUT {
        let clone = home.join(CLONES).join(LAID_OUT_REPO).join(id);
        std::fs::create_dir_all(&clone).expect("the scratch home");
        std::fs::write(clone.join("PRECIOUS.txt"), PRECIOUS).expect("the scratch home");
        let record = home.join(RECORDS).join(id);
        std::fs::create_dir_all(&record).expect("the scratch home");
        std::fs::write(
            record.join("workspace.json"),
            format!("{{\"id\":\"{id}\",\"context\":\"default\"}}\n"),
        )
        .expect("the scratch home");
        worktrees.push(format!(
            "\"{LAID_OUT_REPO}/{id}\":{{\"local_path\":\"{}\"}}",
            clone.display()
        ));
    }
    let registry = home.join(REGISTRY);
    std::fs::create_dir_all(registry.parent().expect("the registry's directory"))
        .expect("the scratch home");
    std::fs::write(
        &registry,
        format!(
            "{{\"version\":2,\"worktrees\":{{{}}}}}\n",
            worktrees.join(",")
        ),
    )
    .expect("the scratch home");
    home
}

/// The directory the probe child may write its own scratch state into — the
/// one holding the log, which is [`record`]'s and is swept with it.
///
/// A probe that needs a path to hand the code under test (a projects cache, a
/// checkout) uses this rather than `HOME`, because `HOME` is the thing being
/// watched for changes and a legitimate write there would read as destruction.
///
/// # Panics
///
/// Outside a probe child, where there is no such directory. Guard with
/// [`is_child`].
pub fn child_scratch() -> PathBuf {
    let log = std::env::var_os(LOG).expect("a probe child has a log");
    PathBuf::from(log)
        .parent()
        .expect("the log lives in the scratch directory")
        .to_path_buf()
}

/// Every file under `root`, by path relative to it, with its contents.
///
/// Contents and not merely names: a workspace whose files were emptied in place
/// is as destroyed as one that was removed, and `rename` shows up as both a
/// disappearance and an appearance.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, at: &Path, into: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, into);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let name = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                into.push((name, bytes));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

/// What changed between two [`tree`] readings, as one line per path.
fn differences(before: &[(String, Vec<u8>)], after: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, bytes) in before {
        match after.iter().find(|(name, _)| name == path) {
            None => changed.push(format!("{path} was removed")),
            Some((_, now)) if now != bytes => changed.push(format!("{path} was rewritten")),
            Some(_) => {}
        }
    }
    for (path, _) in after {
        if !before.iter().any(|(name, _)| name == path) {
            changed.push(format!("{path} appeared"));
        }
    }
    changed
}

/// Write a marked line into the recording log from inside a probe body.
///
/// The log is a *timeline*, not a set: the shims append to it as they are run,
/// so a line the probe writes itself lands between the subprocesses that ran
/// before it and the ones that ran after. That is what lets a probe pin an
/// **ordering** — "the first frame was painted before anything asked the
/// machine a question" is `note` sitting above every argv, and no amount of
/// grepping the source can say that.
///
/// Marked with [`NOTED`], so that a note joins [`Recording::timeline`] and not
/// [`Recording::argv`]: what the probe said about the run is not part of what
/// the run did.
///
/// Silent outside a probe child, for the same reason [`is_child`] exists.
pub fn note(what: &str) {
    use std::io::Write;
    let Some(log) = std::env::var_os(LOG) else {
        return;
    };
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(log)
        .expect("the probe log");
    // One write, terminator included, for the reason [`shim`] gives.
    log.write_all(format!("{NOTED} <{what}>\n").as_bytes())
        .expect("the probe log");
}

/// A `dl --ls --json` listing over four workspaces of this repo, as devlaunch
/// 0.0.24 writes them — one finished ticket, one the planner warns about, one
/// in use, and one whose run stopped between its stages.
///
/// The newest shape on purpose: this is the fixture the *whole reading* is
/// driven through, so it should look like the `dl` this `wf` will be run
/// against next. Every older spelling of `unsaved` has its own row in
/// [`DL_LISTING_UNSAVED`], which is where that field is the subject.
///
/// The fourth exists so the live path can produce a **stall**, not just be
/// asserted to derive one from hand-built state: it is the only row whose
/// container is down while its ticket is claimed, and it is what makes the
/// recorded reading say `stalled 1`.
///
/// Here rather than beside either probe that uses it: the reading's own probe
/// and the picker's drive the same two reads, and a fixture written twice is a
/// fixture that can disagree with itself about what the machine looks like.
///
/// The four ids are also [`LAID_OUT`] as directories in the child's scratch
/// home, so what the code under test can see is exactly what it can be caught
/// destroying.
pub const DL_LISTING: &str = r#"[
  {"id":"wf-129-closed","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-129","state":"Stopped",
   "unsaved":{"nothingToLose":true}},
  {"id":"wf-138-unstarted","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-138","state":"Stopped",
   "unsaved":{"nothingToLose":true}},
  {"id":"wf-137-open","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-137","state":"Running",
   "unsaved":{"nothingToLose":true}},
  {"id":"wf-134-stalled","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-134","state":"Stopped",
   "unsaved":{"nothingToLose":true}}
]"#;

/// Every shape `dl` has ever emitted for `unsaved`, in one listing.
///
/// The top three rows are devlaunch **0.0.24 and newer**: an object with
/// exactly one key. The next two are **0.0.23 and older**, whose field was a
/// bare sentence or `null` — still on real machines, because the floor `wf`
/// holds `dl` to governs what a *launch* may ask of it, while `wf reap` reads a
/// listing from whichever `dl` is on PATH. Then two rows no `dl` has ever
/// emitted: a key from a *later* `dl`
/// than this binary, and the documented key carrying an undocumented payload.
/// One of those carries a documented key **beside** an undocumented sibling,
/// which is the shape a later `dl` produces by adding one field: it must go on
/// being read, because the alternative is every row in the listing refusing at
/// once. The last row is a workspace `dl` did not create, where the field is
/// absent altogether.
///
/// Those two invented rows are the ones that matter most. `parse_workspaces` is
/// all or nothing, so a listing this `wf` cannot fully read is a listing it
/// reads *none* of — and a fixture containing only the shapes already shipped
/// would prove nothing about the release after this one.
///
/// This is a transcription of `dl`'s own documented output rather than
/// something `wf` finds convenient to parse: the object arms and their spelling
/// come from devlaunch's `--ls --json` table, and the whole reason this fixture
/// exists is that the two repos had no executed agreement about this field at
/// all — only prose on each side, which is how the string→object change was
/// able to land unnoticed.
pub const DL_LISTING_UNSAVED: &str = r#"[
  {"id":"wf-1-clean","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-1","state":"Stopped",
   "unsaved":{"nothingToLose":true}},
  {"id":"wf-2-dirty","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-2","state":"Stopped",
   "unsaved":{"wouldLose":"2 uncommitted change(s) (pixi.lock, notes.md) and 1 unpushed commit(s)"}},
  {"id":"wf-3-unreadable","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-3","state":"Stopped",
   "unsaved":{"couldNotTell":"fatal: not a git repository"}},
  {"id":"wf-4-legacy-dirty","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-4","state":"Stopped",
   "unsaved":"1 uncommitted change(s) (pixi.lock)"},
  {"id":"wf-5-legacy-clean","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-5","state":"Stopped","unsaved":null},
  {"id":"wf-6-newer-dl","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-6","state":"Stopped",
   "unsaved":{"someAnswerFromALaterDl":"whatever it means"}},
  {"id":"wf-7-odd-payload","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-7","state":"Stopped",
   "unsaved":{"nothingToLose":false}},
  {"id":"wf-8-sibling-key","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-8","state":"Stopped",
   "unsaved":{"nothingToLose":true,"checkedAt":"2026-08-10T00:00:00Z"}},
  {"id":"not-ours","devlaunch":false,"state":"Stopped"}
]"#;

/// The tracker's answer to the batched question those four nodes raise:
/// #129 closed (a reap), #138 open with nobody on it and no PR (a warning),
/// #137 open and claimed (a keep, and — its container being up — a `▣`),
/// #134 open and claimed with its container down (a keep, and a `⧖`).
pub const GH_FACTS: &str = r#"{"data":{"repository":{
  "i129":{"state":"CLOSED","assignees":{"nodes":[]},
          "closedByPullRequestsReferences":{"nodes":[]}},
  "i137":{"state":"OPEN","assignees":{"nodes":[{"login":"blooop"}]},
          "closedByPullRequestsReferences":{"nodes":[]}},
  "i138":{"state":"OPEN","assignees":{"nodes":[]},
          "closedByPullRequestsReferences":{"nodes":[]}},
  "i134":{"state":"OPEN","assignees":{"nodes":[{"login":"blooop"}]},
          "closedByPullRequestsReferences":{"nodes":[]}}
}}}"#;

/// True when this process *is* a probe child — the guard an `#[ignore]`d probe
/// body needs, because `cargo test -- --ignored` would otherwise run it with
/// no shims on `PATH` and no log to write to.
pub fn is_child() -> bool {
    std::env::var_os(LOG).is_some()
}

/// A directory of this run's own.
///
/// Unique per *call*, not per test: two tests can probe the same child — one
/// asking what it ran and one asking what it said — and the suite runs them at
/// the same time, so a directory named for the child is a directory each of
/// them deletes out from under the other.
fn scratch(test: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let safe: String = test
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = std::env::temp_dir().join(format!(
        "wf-probe-{}-{}-{safe}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the probe's scratch directory");
    dir
}

/// Write one recording shim: append the argv to the log, then print the
/// fixture. One line per call, with newlines inside an argument flattened —
/// `gh api graphql`'s query is a whole multi-line document, and a record that
/// spanned lines could not be read back as one call.
///
/// The line is assembled first and appended **once**, terminator included.
/// Written as two appends — the text, then the newline — two shims running at
/// the same time interleave into one log line, and every assertion that counts
/// the lines becomes a race. An `O_APPEND` write this small is atomic; two are
/// not.
/// The release the shimmed `dl` claims to be, and it must match what the
/// fixtures are written as.
///
/// [`DL_LISTING`] answers `unsaved` with the one-key object, which is a 0.0.24
/// listing, and `reap` reads a *missing* answer differently either side of that
/// release. A shim that listed like 0.0.24 and versioned like 0.0.23 would be
/// evidence about a machine that cannot exist, which is worse than no probe.
///
/// The default, not the only possibility: [`record_as_dl`] takes a version, for
/// probes whose subject *is* which release is on the machine. The pairing rule
/// above is what those callers have to satisfy for themselves — a later release
/// still writes the one-key object, so any version at or above 0.0.24 pairs
/// with [`DL_LISTING`]; an older one has to be given a listing it could have
/// written, which is what [`DL_LISTING_UNSAVED`] collects.
pub const SHIMMED_DL: &str = "0.0.24";

fn shim(dir: &Path, name: &str, dl_version: &str) {
    let path = dir.join(name);
    // `--version` is answered before the fixture, because `dl` answers it with
    // a version rather than with a listing, and a shim that returned the
    // listing to both would be a `dl` that exists nowhere. The recording still
    // happens first, so the call is counted either way.
    //
    // The answer is [`SHIMMED_DL`] for the same reason the fixtures name a
    // release: a probe is only evidence about a machine it could be.
    let body = r#"#!/bin/sh
line=$({
  printf '%s' 'PROGRAM'
  for a in "$@"; do printf ' <%s>' "$a"; done
} | tr '\n' ' ')
printf '%s\n' "$line" >> "$LOGVAR"
if [ "$1" = "--version" ]; then printf '%s\n' 'PROGRAM VERSION'; exit 0; fi
cat 'FIXTURE'
"#
    .replace("PROGRAM", name)
    .replace("VERSION", dl_version)
    .replace("LOGVAR", LOG)
    .replace(
        "FIXTURE",
        &dir.join(format!("{name}.out")).display().to_string(),
    );
    std::fs::write(&path, body).expect("the shim");
    executable(&path);
}

#[cfg(unix)]
fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("the shim").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("the shim's mode");
}

#[cfg(not(unix))]
fn executable(_path: &Path) {
    unimplemented!("the probe shims are `/bin/sh` scripts; `wf` targets unix");
}

/// One of this crate's source files with the comments and the test module
/// stripped — what the code can *do*, rather than what the prose beside it says
/// it does.
///
/// Called as `code_only("reclaim.rs", include_str!("reclaim.rs"))`: the
/// `include_str!` has to stay at the call site, because its path is resolved
/// against the file it is written in, and the name is written beside it so a
/// panic from here names the file at fault rather than this one.
///
/// # What is dropped, and why that is safe
///
/// The test module is dropped because every guard built on this reads a
/// *denylist* — `picker.rs`'s says the file may not name `reap`, and the test
/// that says so has to write `reap` to say it. Keeping the module would make
/// each guard fail on itself.
///
/// Dropping it is only safe if the part dropped is exactly a `#[cfg(test)]`
/// module and nothing else, so that is checked here rather than assumed. Two
/// earlier versions of this check were each defeated by a reviewer, and both
/// are worth recording because the second looked like the fix for the first.
/// The first took the lines *before* the first `mod tests {` and threw the rest
/// away unexamined: a helper written below the test module was invisible to all
/// four guards (only clippy's `items_after_test_module` saw it), and a raw
/// string containing a line reading `mod tests {` cut the file short wherever
/// its author liked — which silenced that lint too. The second read the tail,
/// but only at column 0: it skipped every line starting with a space or a tab,
/// so indenting each item below a raw-string decoy by one space dropped the
/// whole tail out of all four guards again, and only `cargo fmt --check`
/// objected.
///
/// So the whole file is read, and three things are asserted about it:
///
/// 1. exactly one line *starts* a `mod tests {` — a second one anywhere,
///    including inside a string, is the decoy above;
/// 2. it is preceded by `#[cfg(test)]` — so what is dropped is compiled out of
///    every release, not merely named `tests`;
/// 3. counting braces from that line, the depth reaches zero only on the file's
///    last non-blank line. Depth rather than indentation: an item written below
///    the module closes the module's brace above the end of the file, wherever
///    the item itself is indented to.
///
/// What remains uncovered is the *inside* of that module, which cannot run in a
/// shipped binary. The lines above it are returned; the lines below it are what
/// (3) is about, and it is (3) that makes returning only the prefix safe.
///
/// # The braces are counted as text, and the four files pay for that
///
/// Nothing here parses Rust. A `{` or `}` inside a string, a char literal or a
/// comment counts exactly like one in code. That makes this check **fail
/// closed**, and that is the intended trade: an edit it cannot account for is
/// rejected rather than waved through.
///
/// The cost is real, and these four were run against `picker.rs` (the last one
/// also against `main.rs`) — each is an ordinary, security-neutral edit that now
/// fails the guard over whichever file it is made in, with the whole rest of the
/// selection green:
///
/// - `#[allow(clippy::too_many_lines)]` written between the `#[cfg(test)]` and
///   the `mod tests {` — the announcement is no longer the line above;
/// - `#[cfg(all(test, unix))]` in place of `#[cfg(test)]` — likewise;
/// - a second `#[cfg(test)] mod` written *below* the test module — the braces
///   close before the end of the file, which is the whole point of (3);
/// - **a `{` or a `}` that does not balance, written in a string, a char
///   literal or a comment anywhere inside the test module.** Nothing here
///   parses Rust, so an assertion message reading `"expected {"`, or a comment
///   ending `and nothing else }`, moves the depth. This is the widest of the
///   four by a long way — it is a cost on writing ordinary tests, not on
///   arranging modules — and this repository is already paying it: the `// }`
///   at the end of the second entry in `main.rs`'s `reap` list exists only to
///   balance the `{` inside the string above it. Deleting that comment is bins
///   16 / 1, on `main.rs`'s own guard, for an edit that changes nothing.
///
/// Two that read like the same class and are **not** rejected, checked rather
/// than assumed: a trailing `} // end` on the closing brace passes, because
/// what is counted is the brace and not the shape of the line; and a second
/// `#[cfg(test)] mod` written *above* the test module passes, because it is
/// above, which means the denylists read it like any other code. A file with no
/// `mod tests {` line at all fails the first assertion by construction — zero
/// is not one — and that is a shape this repository does not have, so it is
/// stated from the code rather than from a run.
///
/// So `main.rs`, `picker.rs`, `reclaim.rs` and `refresh.rs` are held to one
/// test-module convention — a single `#[cfg(test)] mod tests {` at column 0,
/// last in the file — and a maintainer who departs from it gets a security
/// guard failing on an edit that has nothing to do with security. That is
/// deliberate: the alternative is a check that can be argued out of reading
/// part of the file, and that is how both previous versions were defeated.
///
/// The panic names the file at fault, which is the part that was missing when
/// this check was written — before, a `picker.rs` problem was reported as a
/// `probe.rs` panic. It does **not** always name the line at fault, and the two
/// halves differ: an unbalanced `}` closes the depth early, so the loop fails at
/// the offending line and quotes it (a stray `}` in a comment above
/// `picker.rs`'s `a_session` reported *line 809 of 1037*, which is that
/// comment). An unbalanced `{` never brings the depth back to zero, so nothing
/// fails until the count is checked at the end, and the message can only quote
/// the file's **last** line — deleting `main.rs`'s balancing `// }` reports
/// *line 680*, which is the end of `main.rs` and 183 lines from the edit. In
/// that direction the message says which file and which guard, and the reader
/// finds the brace.
///
/// What a textual count does not reach, measured rather than reasoned: a tail
/// whose braces are *balanced* by braces written inside string literals — one
/// stray `{` in a literal inside the module and one stray `}` in a literal
/// inside the item below it — leaves the depth at zero only on the last line,
/// and all four guards pass, `cargo fmt --check` included. What refuses that
/// edit is clippy's `items_after_test_module`, which CI runs under
/// `-D warnings`; an item below a test module is exactly what that lint is for.
/// So the honest statement is that this function catches the tail written at
/// any indentation, and clippy catches the tail that hand-balances its braces.
///
/// The module is found by its `mod tests {` line rather than by the
/// `#[cfg(test)]` above it, and that is not a detail. `main.rs` declares
/// `#[cfg(test)] mod probe;` among its imports, so a cut at the first
/// `#[cfg(test)]` left thirty lines of `use` statements — a guard over that
/// file would have read no code at all and passed for it.
///
/// # Panics
///
/// If any of the three does not hold, which fails the guard that called it.
pub fn code_only(file: &str, source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let opens: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("mod tests {"))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(
        opens.len(),
        1,
        "{file}: a file guarded by its source text must open exactly one test \
         module at column 0, and this one has {} such lines (a second is how a \
         raw string hides the rest of the file from every guard): {:?}",
        opens.len(),
        opens.iter().map(|at| at + 1).collect::<Vec<_>>()
    );
    let opens = opens[0];
    assert_eq!(
        opens.checked_sub(1).map(|above| lines[above].trim()),
        Some("#[cfg(test)]"),
        "{file}: what is dropped must be announced `#[cfg(test)]`: line {} is {:?}",
        opens,
        opens.checked_sub(1).map(|above| lines[above])
    );
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .expect("a source file has a line");
    // Brace depth, not column. The version this replaced skipped every line
    // starting with a space or a tab, so one space of indentation on each item
    // below a decoy hid the whole tail from all four guards.
    let mut depth = 0usize;
    for (at, line) in lines.iter().enumerate().skip(opens) {
        depth = (depth + line.matches('{').count()).saturating_sub(line.matches('}').count());
        assert!(
            depth > 0 || at == last,
            "{file}: the test module must be this file's last item, and its braces \
             close at line {} of {} — whatever is written below that line is read \
             by no guard here, at any indentation",
            at + 1,
            last + 1
        );
    }
    assert_eq!(
        depth,
        0,
        "{file}: the test module's braces are still {depth} deep at line {}, the \
         file's last — the guards below read only what is above the module, so \
         this file's shape has to be checkable",
        last + 1
    );
    lines[..opens]
        .iter()
        .filter(|line| !line.trim_start().starts_with("//"))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}
