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
//! It pins down three claims, in the order they can fail:
//!
//! 1. **`enter` execs the agent.** `claude` is shimmed to a script that records
//!    its argv and cwd, so the assertion is on what `wf` actually handed the
//!    agent — `--dangerously-skip-permissions "/wayfinder <map> <n>"`, in the
//!    checkout — rather than on what it planned to.
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
//! Needs network, an authenticated `gh`, and a `blooop/wayfinder` checkout with
//! at least one ticket on its map — i.e. this repo.

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

/// A scratch tree of this test's own: a `claude` shim on PATH and a cache
/// directory, so neither the user's PATH nor their real projects cache is
/// touched. Removed on drop, panic or not.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let root = std::env::temp_dir().join(format!("wf-it-exec-{}", std::process::id()));
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

    fn report(&self) -> PathBuf {
        self.0.join("report.txt")
    }

    /// Write the `claude` shim. **No agent is ever launched by this test**: the
    /// shim records what it was handed, the pid it is, and the tty it landed
    /// on, then says so.
    fn write_shim(&self) {
        let script = format!(
            "#!/usr/bin/env bash\n\
             {{ echo \"pid=$$\"; echo \"cwd=$PWD\"; echo \"argv=$*\"; stty -a; }} > {report} 2>&1\n\
             printf '{RAN}\\n'\n",
            report = self.report().display(),
        );
        let claude = self.bin().join("claude");
        std::fs::write(&claude, script).expect("write the claude shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
                .expect("chmod the claude shim");
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &size,
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
                Ok(n) => sink.lock().expect("reader mutex").extend_from_slice(&buf[..n]),
            }
        }
    });
    seen
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

#[test]
fn enter_execs_the_agent_in_the_checkout_and_leaves_no_wf_behind() {
    let scratch = Scratch::new();
    scratch.write_shim();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));

    let (master, slave) = openpty_sized(40, 120);
    let path = format!(
        "{}:{}",
        scratch.bin().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut child = unsafe {
        Command::new(env!("CARGO_BIN_EXE_wf"))
            .current_dir(repo)
            .env("PATH", &path)
            // Keep the user's real projects cache out of it: this run
            // registers a checkout and writes a map seed.
            .env("XDG_CACHE_HOME", scratch.cache())
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

    // `▶` marks the cursor on a *ticket row*, so it cannot be drawn until a map
    // has actually landed — group headers and the empty-list screen have none.
    // Generous, because a cold cache pays for the map search before the fetch.
    wait_for(&seen, "▶", Duration::from_secs(60), "the map to load");

    keys.write_all(b"\r").expect("send enter");
    keys.flush().expect("flush enter");

    let stream = wait_for(&seen, RAN, Duration::from_secs(30), "the agent to run");
    let screen = String::from_utf8_lossy(&stream).into_owned();

    let wf_pid = child.id();
    let status = child.wait().expect("wait for the exec'd agent");
    assert!(status.success(), "the agent exited {status}\n{screen}");

    let report = std::fs::read_to_string(scratch.report()).expect("the shim's report");
    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("the shim records {key:?}\n{report}"))
    };

    // Claim 2: the pid the test spawned *is* the agent. Asserted on the pid and
    // not on the exit status, because the shim exits 0 either way — under
    // spawn-and-wait `wf` would have waited for it and exited 0 too, so a
    // status check passes without distinguishing the two designs at all.
    assert_eq!(
        field("pid=").parse::<u32>().ok(),
        Some(wf_pid),
        "the agent must be the same process as wf, not a child of it"
    );

    // Claim 1: what the agent was actually handed.
    let cwd = field("cwd=");
    let argv = field("argv=");
    assert_eq!(
        Path::new(cwd).canonicalize().ok(),
        repo.canonicalize().ok(),
        "the agent must run in the checkout, not in wf's cwd"
    );
    let (skip, prompt) = argv.split_once(' ').expect("two arguments");
    assert_eq!(skip, "--dangerously-skip-permissions");
    // This repo's map is issue #1; which ticket the cursor was on is the live
    // frontier's business, so only its shape is asserted.
    let ticket = prompt
        .strip_prefix("/wayfinder 1 ")
        .unwrap_or_else(|| panic!("prompt was {prompt:?}"));
    assert!(
        !ticket.is_empty() && ticket.chars().all(|c| c.is_ascii_digit()),
        "prompt was {prompt:?}"
    );

    // Claim 3a: the tty itself came back. `wf` runs raw the whole time it is
    // up, so an agent that finds a raw tty here is one that would have found a
    // dead keyboard in daily use.
    let stty = flags(&report);
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
}
