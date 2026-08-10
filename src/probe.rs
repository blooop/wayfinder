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
//! ran — is as observable as the argv itself.
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

/// The workspace ids [`record`] lays a scratch home out for — the three
/// [`DL_LISTING`] names, so every workspace the code under test can *see* is
/// also a directory it can be caught destroying.
const LAID_OUT: [&str; 3] = ["wf-129-closed", "wf-138-unstarted", "wf-137-open"];

/// The repo those three belong to in [`DL_LISTING`], which is also the
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
    pub argv: Vec<String>,
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
    let dir = scratch(test);
    std::fs::write(dir.join("dl.out"), dl_stdout).expect("the dl fixture");
    std::fs::write(dir.join("gh.out"), gh_stdout).expect("the gh fixture");
    let log = dir.join("argv.log");
    std::fs::write(&log, "").expect("the log");
    shim(&dir, "dl");
    shim(&dir, "gh");
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
    let argv: Vec<String> = std::fs::read_to_string(&log)
        .expect("the log")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
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
    log.write_all(format!("note <{what}>\n").as_bytes())
        .expect("the probe log");
}

/// A `dl --ls --json` listing over three workspaces of this repo, in the shape
/// devlaunch 0.0.21 and newer emit — one finished ticket, one the planner warns
/// about, one in use.
///
/// Here rather than beside either probe that uses it: the reading's own probe
/// and the picker's drive the same two reads, and a fixture written twice is a
/// fixture that can disagree with itself about what the machine looks like.
///
/// The three ids are also [`LAID_OUT`] as directories in the child's scratch
/// home, so what the code under test can see is exactly what it can be caught
/// destroying.
pub const DL_LISTING: &str = r#"[
  {"id":"wf-129-closed","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-129","state":"Stopped"},
  {"id":"wf-138-unstarted","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-138","state":"Stopped"},
  {"id":"wf-137-open","devlaunch":true,"repo":"blooop/wayfinder",
   "branch":"wayfinder/wayfinder-137","state":"Running"}
]"#;

/// The tracker's answer to the batched question those three nodes raise:
/// #129 closed (a reap), #138 open with nobody on it and no PR (a warning),
/// #137 open and claimed (a keep).
pub const GH_FACTS: &str = r#"{"data":{"repository":{
  "i129":{"state":"CLOSED","assignees":{"nodes":[]},
          "closedByPullRequestsReferences":{"nodes":[]}},
  "i137":{"state":"OPEN","assignees":{"nodes":[{"login":"blooop"}]},
          "closedByPullRequestsReferences":{"nodes":[]}},
  "i138":{"state":"OPEN","assignees":{"nodes":[]},
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
fn shim(dir: &Path, name: &str) {
    let path = dir.join(name);
    let body = r#"#!/bin/sh
line=$({
  printf '%s' 'PROGRAM'
  for a in "$@"; do printf ' <%s>' "$a"; done
} | tr '\n' ' ')
printf '%s\n' "$line" >> "$LOGVAR"
cat 'FIXTURE'
"#
    .replace("PROGRAM", name)
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

/// One of this crate's source files with the comments and the whole test module
/// stripped — what the code can *do*, rather than what the prose beside it says
/// it does.
///
/// Called as `code_only(include_str!("reclaim.rs"))`: the `include_str!` has to
/// stay at the call site, because its path is resolved against the file it is
/// written in.
///
/// The test module is found by its `mod tests {` line rather than by the
/// `#[cfg(test)]` above it, and that is not a detail. `main.rs` declares
/// `#[cfg(test)] mod probe;` among its imports, so a cut at the first
/// `#[cfg(test)]` left thirty lines of `use` statements — a guard over that
/// file would have read no code at all and passed for it. Anything smuggled in
/// *below* `mod tests` is caught by clippy's `items_after_test_module` instead.
pub fn code_only(source: &str) -> String {
    source
        .lines()
        .take_while(|line| !line.starts_with("mod tests {"))
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}
