//! End-to-end test for the launch (#34): `enter` runs the agent, and `wf` is
//! gone.
//!
//! This is the only thing that proves the whole path — the real binary, a real
//! tty, a real map off the tracker, a real keypress — and it exists because
//! after Build 7 there is nothing else left guarding it. Everything the unit
//! tests can check about a launch is what `plan` *decided*; the thing that can
//! actually be wrong is what happens between the keypress and the agent's first
//! frame, and that only happens once, in `main`.
//!
//! It pins down seven claims, in the order they can fail:
//!
//! 0. **The container carries the agent, in a workspace of the ticket's
//!    own.** This repo has a `.devcontainer/devcontainer.json`, so its
//!    launches are isolated (#80): `wf` execs `dl`, not `claude`, and hands it
//!    `owner/repo@wayfinder/<repo>-<n>` (#106) — the per-node workspace whose
//!    branch number must be the very ticket the prompt names. Both binaries
//!    are shimmed, and the `dl` shim is what makes the run deterministic —
//!    without it a machine with a real `dl` installed and a machine without
//!    one would take different paths through the same test. The shim records
//!    what `dl` was handed and then runs the quoted command through a real
//!    `bash`, which is the only place the quoting can be checked at all: `dl`
//!    joins everything after `--` and hands it to a shell, so an unquoted
//!    prompt would arrive at `claude` as three arguments and every assertion
//!    in claim 1 would fail. It also answers `dl --version`, because isolation
//!    is conditional on that answer and a shim that stayed silent sent the
//!    whole launch to the host — see `write_shims`.
//! 1. **`enter` execs the agent.** Two enters since the two-step launch (#62):
//!    the first opens the launch picker, the second launches the mode it
//!    opened on — interactive.
//!    `claude` is shimmed to a script that records its argv and cwd, so the
//!    assertion is on what `wf` actually handed the agent —
//!    `--dangerously-skip-permissions "<skill> …"` — rather than on what it
//!    planned to.
//! 2. **`wf` is gone.** The shim is the *same process* as the `wf` that was
//!    spawned: an `exec` replaces the image, so the pid the test is waiting on
//!    exits with the shim's status. Spawn-and-wait would leave a `wf` parent
//!    alive and this would not hold.
//! 3. **The terminal was restored first.** The one ordering that can go wrong,
//!    and the one that is invisible until a human is staring at a dead shell:
//!    the shim reports its own termios, and an agent handed a still-raw tty
//!    fails here instead of in daily use (the #30 failure mode, now guarded
//!    from the other side).
//!
//! 4. **The launch leaves a way back into itself** (#35). The record has to
//!    reach disk *before* the exec, because after it there is no `wf` left to
//!    write anything — so a run that reached the agent and recorded nothing
//!    would silently lose the conversation it had just started.
//! 5. **And coming back rejoins it.** A second `wf`, against the cache the
//!    first one left, opens its picker on a `resume` row and the same
//!    `enter enter` returns to that conversation instead of starting another.
//!    This is the one seam no unit test can reach: `resume_launch` is checked
//!    exhaustively in the library, but only against a record a test handed it.
//! 6. **The launch tells `dl` what `wf` did on the way to it** (#160): the
//!    keystroke that resolved to this exec, and the prewarm this run fired for
//!    the node, both as environment stamps on the `dl` invocation — and on
//!    nothing else `wf` ran. The environment is the one thing an argv
//!    assertion cannot see, and the exec is where it is written, so this is the
//!    only place the seam is observable end to end.
//!
//! Needs network, an authenticated `gh`, and a `blooop/wayfinder` checkout with
//! at least one ticket on its map — i.e. this repo.

// The crate denies `unsafe_code` (see `[lints.rust]` in Cargo.toml) and `src/`
// contains none. This file is the single exception, and it is the reason the
// deny is worth having elsewhere: `openpty`, `fork`/`exec`, and turning the raw
// fds it hands back into `OwnedFd`s are libc calls with no safe equivalent in
// std, and a real pty is the only way to observe the launch at all. Nothing here
// is part of the shipped binary.
#![allow(unsafe_code)]

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the shim prints once it has written its report — the signal that the
/// exec happened and the agent is running.
const RAN: &str = "FAKE-CLAUDE-RAN";

/// The flags that decide whether a terminal is usable at all. Asserted present
/// *and* not negated, because `stty -a` writes the off state as `-flag`.
const COOKED: [&str; 4] = ["echo", "icanon", "isig", "opost"];

/// The line a shim writes before each record, so one report file can hold every
/// invocation of that shim in the order they happened. Not a substring of
/// anything `stty -a` or an argv can produce, because splitting on it is how
/// the records are told apart.
const INVOCATION: &str = "--- shim invocation ---";

/// What the `dl` shim answers `--version` with. See `write_shims`.
const DL_SHIM_VERSION: &str = "9999.0.0";

/// A seam stamp from **somebody else's launch**, exported into `wf`'s own
/// environment before it starts (#160).
///
/// This is not a hypothetical: `dl` sets these for the session it launches, so
/// an agent that runs `wf` inside its own workspace runs it with both already
/// set. Every `dl` child `wf` starts inherits that environment, so the claim
/// "only a launch is stamped" is a claim about a *dirty* environment or it is
/// only a claim about the test rig. The instant is a real one from long before
/// any run of this test, so a leak reads as a handoff that began last year.
const INHERITED_STAMP: &str = "1755194037.000000000";

/// A scratch tree of this test's own: `claude` and `dl` shims on PATH and a
/// cache directory, so neither the user's PATH nor their real projects cache is
/// touched. Removed on drop, panic or not.
struct Scratch(PathBuf);

impl Scratch {
    /// `name` keeps two tests in this binary from sharing a root: they run as
    /// threads of one process, so the pid alone is not unique between them and
    /// the `remove_dir_all` below would delete the other's shims mid-run.
    fn new(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!("wf-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).expect("scratch bin");
        std::fs::create_dir_all(root.join("cache")).expect("scratch cache");
        Scratch(root)
    }

    fn bin(&self) -> PathBuf {
        self.0.join("bin")
    }

    fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }

    /// A Claude Code config directory of this test's own. Not a nicety: the
    /// launch path refreshes the installed skill copies on its way to the exec,
    /// and a test that let it find the real `~/.claude` would be rewriting the
    /// user's prompts as a side effect of running.
    fn claude(&self) -> PathBuf {
        self.0.join("claude")
    }

    fn report(&self) -> PathBuf {
        self.0.join("report.txt")
    }

    /// Where the `dl` shim records the container half of the launch.
    fn dl_report(&self) -> PathBuf {
        self.0.join("dl-report.txt")
    }

    /// Write the two shims. **No agent and no container are ever launched by
    /// this test**: each shim records what it was handed, the pid it is, and
    /// the tty it landed on.
    ///
    /// `dl` then `exec`s the command it was given, through a real `bash` and
    /// with a real `exec`, which is what makes the chain observable: the whole
    /// run stays one process, so claim 2's pid check reaches all the way from
    /// the spawned `wf` to `claude`, and the shell that parses the quoted
    /// command is a genuine one rather than this test's idea of one.
    fn write_shims(&self) {
        // **Appended, one record per invocation**, rather than a file each
        // shim overwrites. A shim is called more than once per run — `wf` asks
        // `dl --version` before it will trust it with a launch — and the
        // resume test calls `claude` once per `wf`. Truncating made "the
        // report" mean "whichever invocation happened to be last", which is
        // how this test came to assert against the version probe instead of
        // the launch and fail claiming `wf` had lost the workspace. Selecting
        // the invocation is now the reader's job: see `invocations`.
        //
        // Assembled first and appended **once**, which is the part that makes
        // sharing one file safe. `wf` reads the listing on a background task
        // while the main thread is asking `dl --version` and later staging the
        // launch, so two `dl` shims can overlap — and a record written as six
        // separate `O_APPEND` writes interleaves with the other's under
        // exactly that overlap, leaving one record holding two `argv=` lines
        // and another none. `field` takes the first match, so the failure is
        // an intermittent red pointing at `wf` having lost the launch. A
        // single small `O_APPEND` write is atomic; six are not. src/probe.rs
        // reached the same conclusion for the same reason.
        //
        // The two seam stamps are recorded beside the argv because the
        // environment is where they travel (#160) — a launch that set them on
        // the wrong child, or left an inherited one on a probe, is invisible in
        // an argv. Always written, empty when unset, so "the variable is not
        // here" is a value this file can read rather than a missing line.

        let record = |report: &Path| {
            format!(
                "line=$({{ echo \"{INVOCATION}\"; echo \"pid=$$\"; echo \"cwd=$PWD\"; \
                 echo \"argv=$*\"; echo \"handoff=$DEVLAUNCH_HANDOFF_T0\"; \
                 echo \"prewarm=$DEVLAUNCH_PREWARM_FIRED_AT\"; stty -a; }} 2>&1)\n\
                 printf '%s\\n' \"$line\" >> {}\n",
                report.display()
            )
        };
        let claude = format!(
            "#!/usr/bin/env bash\n{}printf '{RAN}\\n'\n",
            record(&self.report())
        );
        // `dl <owner/repo@branch> -- <command>`: $1 is the workspace spec,
        // $3 the shell command.
        //
        // The `--version` arm is what keeps this test on the path it is about.
        // `wf` holds `dl` to a version floor and reads a `dl` it cannot place
        // as one that is not there — so a shim that stayed silent got the
        // launch quietly downgraded to the host, and every claim below about
        // per-node workspaces became vacuous rather than false. The number is
        // deliberately far above any floor `wf` will plausibly set: the floor's
        // own rule is what `Devlaunch::from_version_output`'s unit tests are
        // for, and this test should not have to be edited every time it moves.
        let dl = format!(
            "#!/usr/bin/env bash\n{}\
             if [ \"$1\" = \"--version\" ]; then printf 'dl {DL_SHIM_VERSION}\\n'; exit 0; fi\n\
             exec bash -c \"exec $3\"\n",
            record(&self.dl_report())
        );
        for (name, script) in [("claude", claude), ("dl", dl)] {
            let path = self.bin().join(name);
            std::fs::write(&path, script).unwrap_or_else(|e| panic!("write the {name} shim: {e}"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .unwrap_or_else(|e| panic!("chmod the {name} shim: {e}"));
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One `key=value` line out of a **single invocation's** record.
///
/// Takes a record rather than a whole report on purpose: a report holds every
/// invocation of its shim, and `find_map` over the lot would silently answer
/// with the first one — which is the failure this file already had once.
fn field<'a>(record: &'a str, key: &str) -> &'a str {
    record
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("the shim records {key:?}\n{record}"))
}

/// Every field name a serialized context block contains, at any depth —
/// including the tag keys the enums write (`ticket`, `open`). The same shape
/// the unit tests assert the claim-free invariant with — a copy of their
/// helper, since `src`'s test module is not reachable from here, so an edit
/// to one side is owed to the other by hand.
fn keys_of(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(fields) => {
                for (key, child) in fields {
                    keys.insert(key.clone());
                    stack.push(child);
                }
            }
            serde_json::Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    keys
}

/// Every invocation a shim recorded, oldest first.
fn invocations(report: &str) -> Vec<&str> {
    report.split(INVOCATION).skip(1).collect()
}

/// The `n`th invocation of `who`, of exactly `expected` in the whole report.
///
/// The count is not a sanity check bolted onto an index — it is the reason the
/// index means anything. "The invocation" was what this file used to say when
/// there were three, and a reader that takes the first or the last of an
/// unknown number cannot tell a run that did the right thing once from a run
/// that did it twice, or from a run whose second attempt overwrote the first.
fn invocation<'a>(report: &'a str, who: &str, n: usize, expected: usize) -> &'a str {
    let all = invocations(report);
    assert_eq!(
        all.len(),
        expected,
        "{who} ran {} times, not {expected}\n{report}",
        all.len()
    );
    all[n]
}

/// Every invocation where `dl` was asked to **run** something, told apart from
/// the `--version` probe `wf` makes before it will trust `dl` at all.
///
/// `dl <workspace> -- <command>`, so a bare ` -- ` is the whole distinction —
/// and the assertions at the call sites are the ones that check the halves
/// either side of it are the right halves.
fn dl_launches(report: &str) -> Vec<&str> {
    invocations(report)
        .into_iter()
        .filter(|record| field(record, "argv=").contains(" -- "))
        .collect()
}

/// The one invocation where `dl` was asked to run something.
///
/// Insisting on exactly one is part of the claim: a launch is a single handover
/// to a single container, and a `wf` that shelled out to `dl` twice on its way
/// to one agent would be doing something this test has never described.
fn dl_launch(report: &str) -> &str {
    let launches = dl_launches(report);
    assert_eq!(
        launches.len(),
        1,
        "one launch means one `dl <ws> -- <cmd>`; the report holds {}\n{report}",
        launches.len()
    );
    launches[0]
}

/// Every invocation that is *not* a handover — the `--version` probe and the
/// prewarm's `dl <ws> up`. Neither is a launch, so neither may carry a stamp
/// (#160): a `wf` that stamped its probes would have `dl` reporting a hand-over
/// for a question `wf` asked itself.
fn dl_asides(report: &str) -> Vec<&str> {
    invocations(report)
        .into_iter()
        .filter(|record| !field(record, "argv=").contains(" -- "))
        .collect()
}

/// This machine's clock as the seam spells it, for comparing against a stamp.
fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("this machine's clock is past 1970")
        .as_secs_f64()
}

/// One stamp out of a record, as the number it claims to be.
fn stamp(record: &str, key: &str) -> f64 {
    let raw = field(record, key);
    raw.parse()
        .unwrap_or_else(|e| panic!("{key} carries Unix epoch seconds, got {raw:?} ({e})\n{record}"))
}

/// The workspace spec `dl` was pointed at — everything before the first space
/// of `dl <workspace> -- <command>`.
fn dl_workspace(launch: &str) -> &str {
    field(launch, "argv=")
        .split(' ')
        .next()
        .expect("`split` yields at least one field")
}

/// A pty pair with a real window size.
///
/// The size is set explicitly rather than left to `openpty`'s default of 0×0,
/// because a TUI on a zero-row terminal lays out nothing and the test would
/// then wait forever for a row that could never be drawn.
fn openpty_sized(rows: u16, cols: u16) -> (OwnedFd, OwnedFd) {
    let mut master = 0;
    let mut slave = 0;
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    size.ws_row = rows;
    size.ws_col = cols;
    let rc = unsafe {
        libc::openpty(
            std::ptr::from_mut(&mut master),
            std::ptr::from_mut(&mut slave),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_ref(&size),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// Everything the child has written to the pty so far, accumulated off-thread.
///
/// A reader thread rather than a non-blocking read loop: a `read` on the master
/// blocks until the child writes, and the test needs to *wait for* particular
/// output with a deadline, which is a much simpler thing to express against a
/// buffer somebody else is filling.
///
/// Bytes, not a `String`. Decoding each 4096-byte chunk on its own would turn
/// any multi-byte character straddling a read boundary into `U+FFFD` — and the
/// needle this waits on is `▶`. ratatui writes *diffs*, so a row mangled once
/// is never redrawn and the wait would hang out its full deadline. The escape
/// sequences asserted on later are likewise byte facts, not text.
fn spawn_reader(master: OwnedFd) -> Arc<Mutex<Vec<u8>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(master);
        let mut buf = [0u8; 4096];
        loop {
            match file.read(&mut buf) {
                // The child exited, or the pty master gave `EIO` because the
                // last slave closed. Both mean "no more output".
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .expect("reader mutex")
                    .extend_from_slice(&buf[..n]),
            }
        }
    });
    seen
}

/// What counts as quiet, and for how long, before the list counts as settled.
///
/// Measured rather than guessed. Once every map has landed, `wf` writes one
/// empty frame per redraw — 25 bytes of escape sequence, four times a second,
/// and no content at all. A map arriving rewrites the body: the two windows
/// spanning the last two arrivals measured 850 and 551 bytes. [`SETTLED_BYTES`]
/// sits between the two with room on both sides, so neither a slow runner nor
/// an extra idle frame can make an arriving map look like silence.
///
/// [`SETTLED_WINDOWS`] is the part a single threshold cannot do. One quiet
/// window says "no map landed in the last half second", which is *also* true
/// in the gap between two maps — a slow fourth map behind a fast first three
/// would be read as a settled list, which is the whole race back again with a
/// narrower mouth. Consecutive windows turn it into "no map landed in the last
/// second and a half", which is longer than a full four-map load takes.
const SETTLED_WINDOW: Duration = Duration::from_millis(500);
const SETTLED_BYTES: usize = 200;
const SETTLED_WINDOWS: u32 = 3;

/// Wait until the list has stopped rearranging itself under the cursor.
///
/// `wf` streams (#27): the first cluster is drawn as soon as one map lands and
/// the list re-sorts as the others arrive. [`launch_the_first_ticket`]
/// navigates by *position* — two `→` from the project row — so pressing keys
/// the instant a header appears takes whichever node happens to lead at that
/// moment. For the launch test that is merely arbitrary. For the resume test
/// it is a race it can lose: its two runs press the same keys twelve seconds
/// apart and have to reach the *same* node, or the second one finds no resume
/// row and the run being returned to is not the run that was started.
///
/// It is the kind of race that looks like a passing test. Three runs in a row
/// went green while this was being written, and the fourth did not.
///
/// Settled is read as "the terminal has gone quiet" because there is nothing
/// on screen that says it: the `loading maps X/Y` segment *vanishes* when the
/// last map lands rather than announcing itself, and ratatui writes frames as
/// diffs, so no later frame redraws the count line to show it gone. What is
/// observable is volume — see [`SETTLED_BYTES`].
fn wait_until_the_list_settles(seen: &Arc<Mutex<Vec<u8>>>) {
    // Bounded well above the 60s the map itself is given, because this waits
    // for quiet rather than for an event: a `wf` that never stopped drawing
    // should fail here loudly rather than hang the suite.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = seen.lock().expect("reader mutex").len();
    let mut quiet = 0;
    loop {
        std::thread::sleep(SETTLED_WINDOW);
        let now = seen.lock().expect("reader mutex").len();
        // Reset rather than decrement: the run of quiet has to be unbroken, so
        // one map landing in the middle of it starts the count again.
        quiet = if now - last <= SETTLED_BYTES {
            quiet + 1
        } else {
            0
        };
        if quiet >= SETTLED_WINDOWS {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the list never stopped changing: still {} bytes per {SETTLED_WINDOW:?} after 90s",
            now - last
        );
        last = now;
    }
}

/// Wait for `needle` to appear in the child's output, or fail with everything
/// that did arrive — a screen dump is the only useful diagnostic here.
fn wait_for(seen: &Arc<Mutex<Vec<u8>>>, needle: &str, within: Duration, what: &str) -> Vec<u8> {
    let deadline = Instant::now() + within;
    loop {
        let so_far = seen.lock().expect("reader mutex").clone();
        if contains(&so_far, needle.as_bytes()) {
            return so_far;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} ({needle:?} never appeared).\n\
             --- what the terminal got ---\n{}",
            String::from_utf8_lossy(&so_far)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Split `stty -a` output into flag tokens (`echo`, `-icanon`, …), dropping the
/// `key = value` control-character assignments, which are not flags.
fn flags(stty: &str) -> Vec<&str> {
    stty.split([';', '\n'])
        .map(str::trim)
        .filter(|field| !field.contains('=') && !field.is_empty())
        .flat_map(str::split_whitespace)
        .collect()
}

// One test rather than several because it is one *run*: the pty, the shim, the
// two enters and the exec all have to happen in one process's lifetime, and
// three claims read off one launch is what makes them claims about the same
// launch. Splitting it to satisfy a line count would mean three real starts.
#[allow(clippy::too_many_lines)]
#[test]
#[ignore = "live: needs network, gh, a blooop/wayfinder checkout, and a pty"]
fn enter_execs_the_agent_into_a_per_ticket_workspace_and_leaves_no_wf_behind() {
    let scratch = Scratch::new("exec");
    scratch.write_shims();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Claim 4's setup: this repo's skills, installed into a config directory of
    // the test's own, with one copied prompt then vandalised. What the launch
    // does about that is checked after the exec.
    let target = wf::skills::Target::beside(&scratch.claude().join("skills")).expect("a target");
    let bundle = wf::skills::Bundle {
        path: repo.join("skills"),
        found_by: wf::skills::FoundBy::Checkout,
    };
    wf::skills::install(&bundle, &target).expect("install the skills");
    let copied_prompt = target.mirror().join("wf-tdd/SKILL.md");
    std::fs::write(&copied_prompt, "a prompt from an older release\n").expect("vandalise the copy");

    let (master, slave) = openpty_sized(40, 120);
    let path = format!(
        "{}:{}",
        scratch.bin().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // Claim 6's floor: every stamp this run reads has to be an instant inside
    // it, and this is where it starts.
    let started = epoch_now();
    // Read off the binary rather than spelled again, so a renamed variable
    // cannot leave this test polluting the environment with a name nothing
    // reads — which would pass while proving nothing.
    let [t0_var, prewarm_var] = wf::launch::Handoff::variables();
    let mut child = unsafe {
        Command::new(env!("CARGO_BIN_EXE_wf"))
            .current_dir(repo)
            .env("PATH", &path)
            // Keep the user's real projects cache out of it: this run
            // registers a checkout and writes a map seed.
            .env("XDG_CACHE_HOME", scratch.cache())
            .env("CLAUDE_CONFIG_DIR", scratch.claude())
            // Opt into the prewarm, so claim 6's *second* stamp has something
            // to describe: the first enter fires `dl <ws> up` at the shim, and
            // the launch that follows is the only thing that can tell `dl`
            // when that happened. No container is built — the `up` reaches the
            // same recording shim as everything else here.
            .env("WF_PREWARM", "1")
            // Start `wf` inside a launch that is not this one — see
            // `INHERITED_STAMP`. Every `dl` this run starts that is not the
            // exec has to clear these rather than pass them on.
            .env(t0_var, INHERITED_STAMP)
            .env(prewarm_var, INHERITED_STAMP)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
            .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
            .stderr(Stdio::from(slave.try_clone().expect("dup slave")))
            .pre_exec(|| {
                // Give `wf` a session and a controlling terminal of its own, so
                // it is a tty program rather than a process merely holding a
                // tty fd — that is what makes raw mode and its restore real.
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .expect("spawn wf under a pty")
    };
    // The parent must not keep a slave open, or the reader never sees EOF.
    let mut keys = std::fs::File::from(master.try_clone().expect("dup master"));
    drop(slave);
    let seen = spawn_reader(master);

    // The same descent the resume test makes, through the same helper: wait
    // for the list, let it settle, two `→` onto the first ticket, `enter` to
    // stage and `enter` to take the leading row. Nothing has been launched on
    // this node from this cache before — the cache is the test's own and this
    // run is the first — so the row it opens on is `interactive`, today's
    // default, rather than a way back.
    //
    // Shared rather than spelled out twice. The two tests have to press the
    // *same* keys against the *same* screen or they are not driving one
    // product, and this navigation has already grown a settle wait and a
    // two-step launch since it was written.
    launch_the_first_ticket(&mut keys, &seen, "▶ interactive");

    let stream = wait_for(&seen, RAN, Duration::from_secs(30), "the agent to run");
    let screen = String::from_utf8_lossy(&stream).into_owned();

    let wf_pid = child.id();
    let status = child.wait().expect("wait for the exec'd agent");
    assert!(status.success(), "the agent exited {status}\n{screen}");

    let claude_report =
        std::fs::read_to_string(scratch.report()).expect("the claude shim's report");
    // One `wf`, one agent.
    let report = invocation(&claude_report, "claude", 0, 1);
    let dl_report = std::fs::read_to_string(scratch.dl_report()).expect("the dl shim's report");

    // Claim 0: `wf` handed the whole agent command to `dl`, as one shell
    // command after `--`, with a **per-node workspace** —
    // `owner/repo@wayfinder/<repo>-<n>`, never the checkout path — as the
    // workspace (#106): the branch is what buys N tickets N containers, and
    // what keeps every agent out of the human's tree. The prompt is the
    // argument that has to survive a shell, so it is the one that has to be
    // quoted.
    //
    // That `dl` got the launch at all is the version floor's doing, and it is
    // asserted *here* — by there being a launch to read — rather than by
    // looking for a `--version` record. The probe is memoized and the startup
    // listing fires it, so a `--version` in this report says nothing about
    // whether the launch path consulted it: a `wf` with the floor check
    // deleted writes exactly the same record. The floor's own two-sided rule
    // is pinned hermetically, where both versions can be driven —
    // `the_version_floor_is_what_decides_whether_a_launch_is_isolated` in
    // src/picker.rs.
    let dl_argv = field(dl_launch(&dl_report), "argv=");
    let (workspace, rest) = dl_argv.split_once(' ').expect("a workspace and a command");
    let workspace_ticket = workspace
        .strip_prefix("blooop/wayfinder@wayfinder/wayfinder-")
        .unwrap_or_else(|| {
            panic!("dl must be pointed at this repo's per-node workspace, got {workspace:?}")
        });
    assert!(
        !workspace_ticket.is_empty() && workspace_ticket.chars().all(|c| c.is_ascii_digit()),
        "the workspace branch ends in the node's number, got {workspace:?}"
    );
    let command = rest
        .strip_prefix("-- ")
        .unwrap_or_else(|| panic!("the agent command follows a bare `--`: {dl_argv:?}"));
    assert!(
        command.starts_with("'claude' '--dangerously-skip-permissions' '/"),
        "every argument of the agent command must reach the container's shell \
         quoted, or the prompt arrives as several: {command:?}"
    );

    // Claim 2: the pid the test spawned *is* the agent. Asserted on the pid and
    // not on the exit status, because the shim exits 0 either way — under
    // spawn-and-wait `wf` would have waited for it and exited 0 too, so a
    // status check passes without distinguishing the two designs at all.
    assert_eq!(
        field(report, "pid=").parse::<u32>().ok(),
        Some(wf_pid),
        "the agent must be the same process as wf, not a child of it"
    );

    // Claim 1: what the agent was actually handed. The cwd is the picked
    // checkout because that is where `wf` chdir'd before the exec — in a real
    // launch `dl` re-homes the agent into the workspace clone; the shim runs
    // it where it stands, so what is observable here is the chdir.
    let cwd = field(report, "cwd=");
    let argv = field(report, "argv=");
    assert_eq!(
        Path::new(cwd).canonicalize().ok(),
        repo.canonicalize().ok(),
        "wf must exec from the picked checkout, not from its own cwd"
    );
    let (skip, prompt) = argv.split_once(' ').expect("two arguments");
    assert_eq!(skip, "--dangerously-skip-permissions");
    // This repo keeps several maps open (#50), and which cluster — and which
    // ticket, at which stage — the cursor landed on is the live tracker's
    // business. So only the prompt's shape is asserted: one of the interactive
    // routes (#61), with its numeric arguments and no steering suffix (the
    // line was empty). `/wf-auto` cannot appear — that needs the word
    // `auto` typed — and neither can a bare map launch, since the default
    // cursor skips cluster headers (#96).
    // A launched skill is handed the context `wf` already had (#124), as a
    // one-line JSON block after the skill's own arguments. Split it off before
    // the numeric checks below: it is the rest of the prompt, and the steering
    // line was empty, so nothing follows it.
    let (invocation, ctx) = prompt
        .split_once(" ctx: ")
        .unwrap_or_else(|| panic!("a launched skill carries its context: {prompt:?}"));
    let ctx: serde_json::Value = serde_json::from_str(ctx)
        .unwrap_or_else(|e| panic!("the context block is JSON: {e} in {prompt:?}"));
    assert_eq!(ctx["v"], 1, "the schema this binary writes");
    assert_eq!(
        ctx["repo"], "blooop/wayfinder",
        "the context is anchored to the repo the launch was picked in"
    );
    // The claim is unrepresentable in the schema, which is what makes the
    // block safe to orient from: whatever the live tracker said about this
    // node, no assignee can have reached the agent. Asserted on the block's
    // *field names*, the shape the unit tests use (#133) — the substring scan
    // this replaces could not tell a key from a value, so a ticket whose live
    // title happened to contain "claim" failed it spuriously while a field a
    // blacklist never thought of sailed through. The live node varies, so
    // instead of the unit tests' pinned set, every key must be one the v1
    // schema writes and the aim-independent core must be present. One caveat
    // said plainly rather than implied away: this file is excluded from the
    // CI chain by design (AGENTS.md) and guards only when run by name.
    let keys = keys_of(&ctx);
    let schema: std::collections::BTreeSet<&str> = [
        "v",
        "repo",
        "map",
        "number",
        "title",
        "aim",
        "ticket",
        "ticket_type",
        "stage",
        "prs",
        "status",
        "open",
        "checks",
        "review",
    ]
    .into();
    for key in &keys {
        assert!(
            schema.contains(key.as_str()),
            "{key:?} is not a field the v1 schema writes: {ctx}"
        );
    }
    for always in ["v", "repo", "map", "number", "title", "aim"] {
        assert!(keys.contains(always), "{always:?} missing from {ctx}");
    }
    for forbidden in [
        "assignee",
        "assignees",
        "claim",
        "frontier",
        "blocked_by",
        "needs",
    ] {
        assert!(
            !schema.contains(forbidden),
            "{forbidden:?} must be unrepresentable in the handed context"
        );
    }
    let (skill, numbers) = invocation.split_once(' ').expect("a skill and arguments");
    let halves: Vec<&str> = match skill {
        "/wf" => numbers.splitn(2, ' ').collect(),
        "/wf-tdd" | "/wf-review" => vec![numbers],
        other => panic!("unroutable skill {other:?} in prompt {prompt:?}"),
    };
    for half in &halves {
        assert!(
            !half.is_empty() && half.chars().all(|c| c.is_ascii_digit()),
            "prompt was {prompt:?}"
        );
    }
    if skill == "/wf" {
        assert_eq!(halves.len(), 2, "prompt was {prompt:?}");
    }
    // The two halves of the launch agree: the workspace `dl` was pointed at is
    // named by the same node the prompt hands the skill — the last numeric
    // argument (the ticket, or the map when the map is the whole subject).
    assert_eq!(
        halves.last().copied(),
        Some(workspace_ticket),
        "the workspace branch and the prompt must name the same node: \
         {workspace:?} vs {prompt:?}"
    );

    // Claim 5: the launch left a way back into itself (#35). The record has to
    // be on disk *before* the exec — after it there is no `wf` left to write
    // anything — so a run that reached the agent and recorded nothing would
    // lose the conversation it just started. It names the same node as the
    // workspace and the prompt, in the tree the agent was exec'd into.
    let cache_file = scratch.cache().join("wf").join("projects.json");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_file).expect("the projects cache"))
            .expect("the cache is JSON");
    let sessions = written["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("the launch recorded no session: {written}"));
    let session = sessions
        .iter()
        .find(|s| s["repo"] == "blooop/wayfinder")
        .unwrap_or_else(|| panic!("no session for the launched repo: {written}"));
    assert_eq!(
        session["number"].as_u64().map(|n| n.to_string()).as_deref(),
        Some(workspace_ticket),
        "the recorded node must be the one that launched: {session}"
    );
    assert_eq!(session["agent"], "claude");
    assert_eq!(
        Path::new(session["checkout"].as_str().expect("a checkout path"))
            .canonicalize()
            .ok(),
        repo.canonicalize().ok(),
        "the resume must point at the tree the agent actually ran in"
    );

    // Claim 3a: the tty itself came back. `wf` runs raw the whole time it is
    // up, so an agent that finds a raw tty here is one that would have found a
    // dead keyboard in daily use.
    let stty = flags(report);
    for flag in COOKED {
        assert!(
            stty.contains(&flag),
            "the agent inherited a tty with {flag} off — wf exec'd before restoring it.\n\
             stty: {stty:?}"
        );
    }

    // Claim 3b: and so did the *screen*. Alternate-screen and cursor visibility
    // are DEC private modes held by the emulator, not termios — `stty` cannot
    // see either, so the check above passes with the cursor still hidden. That
    // is not hypothetical: every frame writes `?25l` (nothing in the picker
    // positions a cursor) and the only thing that writes `?25h` back is
    // `Terminal`'s `Drop`, which an `exec` skips. Both modes outlive the
    // process, so whatever `wf` leaves set is what the agent runs inside.
    let handover = &stream[..stream
        .windows(RAN.len())
        .position(|w| w == RAN.as_bytes())
        .expect("the marker is in the stream")];
    assert!(
        contains(handover, b"\x1b[?1049l"),
        "wf never left the alternate screen before handing over"
    );
    assert!(
        contains(handover, b"\x1b[?25h"),
        "wf never made the cursor visible again before handing over — \
         the agent inherits an invisible cursor"
    );

    // Claim 3c: the terminal came back with a *name* on it — the node that was
    // launched — written by `wf` and by nothing else here.
    //
    // Asserted on `handover`, the stream up to the agent's own first byte,
    // which is what makes it `wf`'s escape rather than something the shim or
    // the shell could have written. On a real launch `dl` writes its own name
    // over this one a moment later; the shim writes none, so what is observable
    // here is exactly `wf`'s half.
    let named = format!("\x1b]2;wayfinder#{workspace_ticket}\x07");
    assert!(
        contains(handover, named.as_bytes()),
        "wf must name the terminal after the node it launched, expected {named:?}"
    );
    // The other half of the feature is not `wf`'s on this arm and so is not
    // observable here: inside a container the agent's own titling is quieted
    // from the login profile `dl` writes, which every `bash -lc` payload reads
    // (devlaunch#436). What `wf` does on the *host* arm — the same variable, set
    // on the child it becomes — has no container and no `dl` in it, so it is
    // pinned in the library instead
    // (`a_host_launch_is_the_one_that_has_to_quiet_the_agent_itself`).

    // Claim 4: the agent can actually *run* the skill it was just handed. The
    // prompt is a slash command, so a link the agent cannot resolve is not an
    // error anywhere near here — it is `Unknown command: /wf-tdd` inside the
    // session (#107). Two halves, both only observable on a real launch: the
    // link is relative, so it still resolves after `dl` mounts `~/.claude` into
    // the container at another path; and the copy behind it was brought back in
    // step with this build's `skills/` on the way past, which is what stops a
    // copy from being a thing that goes stale.
    assert_eq!(
        std::fs::read_link(target.links().join("wf-tdd")).ok(),
        Some(PathBuf::from("../wf-skills/wf-tdd")),
        "the installed link must name no home directory, or the container's \
         copy of it dangles"
    );
    assert_eq!(
        std::fs::read_to_string(&copied_prompt).ok(),
        std::fs::read_to_string(repo.join("skills/wf-tdd/SKILL.md")).ok(),
        "the launch must refresh the prompt it is about to exec"
    );

    // Claim 6: the launch told `dl` when the keystroke that resolved to it
    // landed, and when this node's prewarm went out (#160). Read off the
    // *environment* the shim was handed, which is the only place this can be
    // observed at all: the stamps are not in the argv, and the exec is where
    // they are applied.
    let launch = dl_launch(&dl_report);
    let t0 = stamp(launch, "handoff=");
    let fired = stamp(launch, "prewarm=");
    let now = epoch_now();
    assert!(
        (started..=now).contains(&t0),
        "the keystroke stamp must be an instant inside this run: \
         {t0} against {started}..={now}"
    );
    // The prewarm went out at the *first* enter and the keystroke stamp is the
    // second, so this ordering is the two halves of the two-step launch,
    // observed from the far side of the exec. It is also what makes the pair
    // worth sending: `dl` subtracts them to see how much head start it had.
    assert!(
        (started..=t0).contains(&fired),
        "the prewarm fired before the keystroke that resolved to the launch: \
         {fired} against {started}..={t0}"
    );
    // And nothing that is not a launch carries either stamp. `wf` asks `dl` its
    // version and fires the prewarm's `dl <ws> up` from the same process with
    // the same environment; a stamp on those would have `dl` report a hand-over
    // for a question `wf` asked itself, or for the warm-up the stamp is *about*.
    //
    // This `wf` was started with both stamps already set (`INHERITED_STAMP`),
    // which is what makes the assertion a claim about `wf` rather than about
    // the test rig: inheriting is the default, so a `wf` that merely declines
    // to *set* a stamp on its asides passes this only in a clean environment,
    // and an agent's shell inside a workspace is not one. Empty here means
    // every such child was scrubbed on the way out.
    let asides = dl_asides(&dl_report);
    assert!(
        asides.len() >= 2,
        "this run probes `dl --version` and fires a prewarm, so there are \
         asides to check\n{dl_report}"
    );
    for aside in asides {
        assert_eq!(
            (field(aside, "handoff="), field(aside, "prewarm=")),
            ("", ""),
            "only a launch is stamped, and this is not one:\n{aside}"
        );
    }
}

/// Spawn `wf` under a fresh pty in `scratch`, returning the child, a handle to
/// write keys into it, and the accumulating output.
///
/// Extracted for the round trip below, which needs two runs sharing one cache —
/// the first to leave a conversation behind, the second to find it. The test
/// above keeps its own inline copy: it asserts on the pid of the process it
/// spawned, and threading that through a helper would put the thing being
/// proved behind an abstraction.
fn spawn_wf(
    scratch: &Scratch,
    cwd: &Path,
) -> (std::process::Child, std::fs::File, Arc<Mutex<Vec<u8>>>) {
    let (master, slave) = openpty_sized(40, 120);
    let path = format!(
        "{}:{}",
        scratch.bin().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let child = unsafe {
        Command::new(env!("CARGO_BIN_EXE_wf"))
            .current_dir(cwd)
            .env("PATH", &path)
            .env("XDG_CACHE_HOME", scratch.cache())
            .env("CLAUDE_CONFIG_DIR", scratch.claude())
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
            .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
            .stderr(Stdio::from(slave.try_clone().expect("dup slave")))
            .pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .expect("spawn wf under a pty")
    };
    let keys = std::fs::File::from(master.try_clone().expect("dup master"));
    drop(slave);
    (child, keys, spawn_reader(master))
}

/// Walk from the project row down to the first cluster's first ticket, stage
/// it, and take the picker's leading row.
///
/// Every launch in this file goes through here, so the same keys reach the same
/// screen in all three runs. For the resume test that is load-bearing twice
/// over: its two runs have to land on the same node, or the second is a fresh
/// launch somewhere else rather than a *return* to the first.
///
/// A *cluster header* cannot be drawn until a map has landed, which is what the
/// first wait is for. Waiting on `▶` alone would not do it any more: since #135
/// the cursor starts on the project row, which is drawn from the local cache on
/// the very first frame and so says nothing about the fetch. The needle is one
/// contiguous write — ratatui emits diffs, so `" #"` before a ticket number is
/// split by the cursor-positioning escape between them and never appears in the
/// stream. Generous, because a cold cache pays for the map search before the
/// fetch.
///
/// Then [`wait_until_the_list_settles`], because `wf` opens standing on the
/// project row (#135) and `→` steps forward one stop at a time — two of them go
/// project → cluster header → its first ticket, and that counting only holds
/// against a list that has stopped moving.
fn launch_the_first_ticket(keys: &mut std::fs::File, seen: &Arc<Mutex<Vec<u8>>>, leading: &str) {
    wait_for(
        seen,
        "▌ wayfinder · ",
        Duration::from_secs(60),
        "the map to load",
    );
    wait_until_the_list_settles(seen);
    for _ in 0..2 {
        keys.write_all(b"\x1b[C").expect("send right");
    }
    keys.flush().expect("flush the descent");
    keys.write_all(b"\r").expect("send enter");
    keys.flush().expect("flush enter");
    wait_for(seen, leading, Duration::from_secs(10), "the launch picker");
    keys.write_all(b"\r").expect("send the second enter");
    keys.flush().expect("flush the second enter");
}

/// The round trip (#35): launch a node, come back, and rejoin the very
/// conversation that launch started.
///
/// The one seam no unit test can reach. `launch::resume_launch` is checked
/// exhaustively in the library, but what it is checked *against* is a `Resume`
/// the test handed it. Here the record is written by one real `wf` on its way
/// out of the process and read by a second real `wf` on its way up — through an
/// actual file, an actual serde round trip, and the cache-loading path in
/// `main` — so a resume that is offered but points nowhere, or is not offered
/// at all, fails here and nowhere else.
///
/// Both runs press the same keys. The second one presses them against a screen
/// that now has a resume row leading its picker, so the same `enter enter` that
/// *started* the work is the one that returns to it — which is the claim the
/// whole feature is for.
#[test]
#[ignore = "live: needs network, gh, a blooop/wayfinder checkout, and a pty"]
fn coming_back_to_a_node_rejoins_the_conversation_the_first_launch_started() {
    let scratch = Scratch::new("resume");
    scratch.write_shims();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Run one: an ordinary launch, which leaves the record behind.
    let (mut child, mut keys, seen) = spawn_wf(&scratch, repo);
    launch_the_first_ticket(&mut keys, &seen, "▶ interactive");
    wait_for(&seen, RAN, Duration::from_secs(30), "the agent to run");
    assert!(child.wait().expect("wait").success());
    let first = std::fs::read_to_string(scratch.report()).expect("the first run's report");
    let launched = field(invocation(&first, "claude", 0, 1), "argv=").to_string();
    assert!(
        launched.contains("/wf"),
        "the first run must be a fresh skill launch: {launched}"
    );

    // Run two: the same keys, against a cache that now knows about that node.
    // The picker leads with `resume`, so the second enter takes it.
    let (mut child, mut keys, seen) = spawn_wf(&scratch, repo);
    launch_the_first_ticket(&mut keys, &seen, "▶ resume");
    wait_for(&seen, RAN, Duration::from_secs(30), "the agent to run");
    assert!(child.wait().expect("wait").success());

    // Both runs append to the one report, so "the second run's argv" is the
    // second of exactly two records — the count is itself the claim that the
    // resume ran the agent again, once.
    let second = std::fs::read_to_string(scratch.report()).expect("the second run's report");
    let resumed = field(invocation(&second, "claude", 1, 2), "argv=");
    // The agent's own way back, and *only* that: no skill, and no `ctx:` block
    // — the conversation being rejoined worked all of that out already.
    assert_eq!(
        resumed, "--continue --dangerously-skip-permissions",
        "the second run must rejoin rather than start again"
    );

    // And it went back into the same container the first launch ran in, which
    // is what makes it the same conversation: both agents key their history by
    // cwd, and that cwd is inside this workspace.
    //
    // Asserted as an *equality between the two launches* rather than as a
    // prefix match on the second. The prefix is satisfied by any node's
    // workspace, so on its own it would let the resume re-enter the wrong
    // container and still pass — and re-entering the wrong container is
    // precisely the way this feature breaks while looking like it works.
    let dl = std::fs::read_to_string(scratch.dl_report()).expect("the dl shim's report");
    let handovers = dl_launches(&dl);
    assert_eq!(
        handovers.len(),
        2,
        "two runs reached the agent, so two handovers to `dl`; got {}\n{dl}",
        handovers.len()
    );
    let (first_ws, resumed_ws) = (dl_workspace(handovers[0]), dl_workspace(handovers[1]));
    assert!(
        resumed_ws.starts_with("blooop/wayfinder@wayfinder/wayfinder-"),
        "the resume must re-enter a per-node workspace, got {resumed_ws:?}"
    );
    assert_eq!(
        resumed_ws, first_ws,
        "the resume must re-enter the *same* workspace the first launch ran in"
    );
}
