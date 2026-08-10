//! Test scaffolding shared by more than one module.
//!
//! Two things live here, both compiled into the library's test build **and**
//! the binary's (`#[cfg(test)] mod probe;` in `lib.rs` and in `main.rs`), which
//! is the only way a piece of test support can be reached from both crates.
//!
//! [`record`] is the important one. `wf`'s dangerous edges are all subprocess
//! calls — `dl` and `gh` — and a subprocess call is *observable*: put a
//! recording shim first on `PATH` and every argv the code under test reached
//! for is written down. That turns "this path cannot delete anything" from a
//! grep over the source into a fact about a run, which is what #137 asks for
//! and what a grep cannot give: a mutation that names none of the forbidden
//! tokens still has to run `dl` to destroy a workspace, and running `dl` is
//! exactly what this sees.
//!
//! The shims have to be first on the `PATH` of the process doing the work, and
//! `PATH` is per-process, so the work happens in a **child**: an `#[ignore]`d
//! test in this same binary, named by `--exact` and run with `--ignored`. That
//! keeps the environment surgery out of the parent — where it would race every
//! other test in the binary — and costs one process spawn.
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

/// What a probe run saw.
#[derive(Debug)]
pub struct Recording {
    /// One line per subprocess the child ran, in order: the program name and
    /// then each argument in angle brackets, so an argument containing a space
    /// cannot be mistaken for two.
    pub argv: Vec<String>,
    /// Everything the child printed.
    pub stdout: String,
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

    /// Fail if anything recorded could have destroyed a workspace.
    ///
    /// Deliberately not a list of function names — the point of watching argv
    /// rather than source text is that it does not matter *how* the deletion
    /// was spelt in Rust. `dl <ws> rm` is the only thing that removes a
    /// workspace, `--force` the only waiver, and both are visible here however
    /// they were reached.
    pub fn destroyed_nothing(&self) {
        for line in &self.argv {
            for forbidden in ["<rm>", "<--force>", "<remove>", "<delete>"] {
                assert!(
                    !line.contains(forbidden),
                    "this path must not be able to destroy a workspace, and it ran: {line}"
                );
            }
        }
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
    // Read the log *before* asserting anything, so that the scratch directory
    // is swept on the way out of a failing run too — a probe that leaked a
    // directory per failure would fill `/tmp` on exactly the day someone is
    // running it in a loop.
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
    Recording { argv, stdout }
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
    writeln!(log, "note <{what}>").expect("the probe log");
}

/// A `dl --ls --json` listing over three workspaces of this repo, in the shape
/// devlaunch 0.0.21 and newer emit — one finished ticket, one the planner warns
/// about, one in use.
///
/// Here rather than beside either probe that uses it: the reading's own probe
/// and the picker's drive the same two reads, and a fixture written twice is a
/// fixture that can disagree with itself about what the machine looks like.
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
fn shim(dir: &Path, name: &str) {
    let path = dir.join(name);
    let body = r#"#!/bin/sh
{
  printf '%s' 'PROGRAM'
  for a in "$@"; do printf ' <%s>' "$a"; done
} | tr '\n' ' ' >> "$LOGVAR"
printf '\n' >> "$LOGVAR"
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
