//! What the terminal is called while the agent a launch started is running.
//!
//! A `wf` launch is a session you switch away from: N tickets are N terminals,
//! and the only thing that tells them apart from outside is the window — or
//! multiplexer tab — name. So a launch names it, after the node it launched
//! (`wayfinder#191`, [`crate::launch::Launch::key`]), with the OSC 2 escape
//! every terminal this repo runs in understands, written once on the way out of
//! the process.
//!
//! ## Why the agent has to be quieted for that to mean anything
//!
//! Claude Code writes the terminal title continuously while it runs, from its
//! own read of what the session is doing. A terminal takes the last writer's
//! word for it, so an escape `wf` writes and an escape the agent writes are not
//! two signals but **one contest**, and the agent wins every round after the
//! first: the name a launch wrote is gone inside a second. Naming the terminal
//! and leaving the agent to name it too is therefore not half a feature, it is
//! none of one — which is what blooop/devlaunch#436 measured on `dl`'s own
//! title, and what the variable in [`crate::launch::Agent::quiet_title_var`]
//! settles.
//!
//! That is why one value decides both halves here. A [`TerminalTitle::Off`]
//! writes no escape *and* quiets nothing: a launch that is not naming the
//! terminal has no name to protect, and an agent silenced with nothing put in
//! its place leaves the window wearing whatever a session hours ago called it,
//! which is worse than a name that moves.
//!
//! Both halves are the **host arm's** alone. An isolated launch hands the
//! terminal to `dl`, which names it after the workspace and — since
//! blooop/devlaunch#436 (`dl` 0.14.0) — exports the same suppression into the
//! container's login profile, so an escape from `wf` there would be replaced
//! within the second and a suppression from `wf` would be a second mechanism
//! for one job. See [`crate::launch::Launch::terminal_title`], and the README
//! for the two `dl`-side cases that leaves alone.
//!
//! ## `WF_NO_TITLE`
//!
//! One variable, governing the whole feature — the escape and the quieting
//! both, for the reason above. It is an opt-*out*, so an unrecognised value
//! reads as "set": the cost of being wrong is one escape sequence nobody
//! wanted, which is the cheap direction. This is deliberately the opposite rule
//! to [`crate::launch::prewarm_enabled`]'s allowlist, and the asymmetry is the
//! cost of being wrong there — `WF_PREWARM` opts *in* to clones and containers,
//! so a value nobody anticipated must not start building any.
//!
//! `dl` has the same switch over its own three pieces (`DEVLAUNCH_NO_TITLE`:
//! its escape, the `PS1` line, and the profile export above), and `wf`
//! deliberately does not read it — it governs a terminal `dl` has taken over,
//! and this one governs the arm `wf` keeps. With it set, a container session is
//! the agent's to name, which is what it was before any of this.

use std::ffi::OsStr;
use std::io::{IsTerminal, Write};

/// `WF_NO_TITLE`: do not name the terminal after the launch, and leave the
/// agent's own titling alone. See the module docs for why it is one variable
/// over both halves and why an unrecognised value turns it off.
pub const DISABLE_VAR: &str = "WF_NO_TITLE";

/// The spellings that leave the feature **on** despite the variable being set —
/// what somebody who once turned it off writes to turn it back on without
/// unsetting anything. Everything else, including a value this list never
/// anticipated, turns it off.
const FALSEY: [&str; 4] = ["", "0", "false", "no"];

/// One complete OSC 2 sequence — `ESC ] 2 ; <name> BEL` — built from a name
/// [`sanitize`] has already filtered.
///
/// A newtype with a private field, so [`TerminalTitle::named`] is the only way
/// to have one. The variant used to carry a bare `String` whose doc comment
/// *said* "a complete OSC 2 sequence": a claim about a payload anything in the
/// crate could fill with anything, which put the module's one guarantee — that
/// no name reaches a terminal carrying escapes of its own — in a comment rather
/// than in the types. The arms themselves stay public because callers construct
/// [`TerminalTitle::Off`] to mean "not titling".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Osc(String);

impl Osc {
    /// The bytes to write.
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// What this launch has to say about the terminal's name.
///
/// Two arms rather than an `Option<Osc>` so the type says what it is at
/// every site that carries it, including the one that only asks whether
/// anything is named at all ([`TerminalTitle::is_named`], which is how the
/// quieting decision is taken from the same value as the escape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTitle {
    /// Write exactly this, terminator and all — see [`Osc`].
    Write(Osc),
    /// Write nothing, and let the agent name the terminal however it likes.
    Off,
}

impl TerminalTitle {
    /// What this process wants written for `name`, read off the real
    /// environment and the real stderr.
    ///
    /// The only impure entry point, and it does nothing but gather the two
    /// inputs [`TerminalTitle::named`] decides from — so every rule is testable
    /// without mutating an environment the rest of the suite shares.
    ///
    /// `var_os` rather than `var`: an environment variable is bytes, and a
    /// `var().ok()` reads one that is not UTF-8 as *unset*, which for an
    /// opt-out means honouring `WF_NO_TITLE=$'\xff1'` by titling anyway. The
    /// undecodable case belongs to whichever answer is safe, and here that is
    /// "they asked for it off".
    pub fn wanted(name: &str) -> TerminalTitle {
        TerminalTitle::named(
            name,
            std::env::var_os(DISABLE_VAR).as_deref(),
            std::io::stderr().is_terminal(),
        )
    }

    /// The rule: name the terminal unless the variable says not to, there is no
    /// terminal to name, or nothing survives [`sanitize`].
    ///
    /// `terminal` is stderr's, because stderr is the stream the escape is
    /// written to — `wf > log` is still a session in a window, and `wf` piped
    /// into something is not a launch at all, since the picker cannot be driven
    /// without a tty.
    pub fn named(name: &str, no_title: Option<&OsStr>, terminal: bool) -> TerminalTitle {
        if switched_off(no_title) || !terminal {
            return TerminalTitle::Off;
        }
        match sanitize(name) {
            Some(text) => TerminalTitle::Write(Osc(format!("\x1b]2;{text}\x07"))),
            None => TerminalTitle::Off,
        }
    }

    /// The bytes to write, or nothing. Borrowed: the caller writes it and drops
    /// it.
    pub fn osc(&self) -> Option<&str> {
        match self {
            TerminalTitle::Write(osc) => Some(osc.as_str()),
            TerminalTitle::Off => None,
        }
    }

    /// Is this launch naming the terminal? The same answer that decides whether
    /// the agent's own titling is quieted — see the module docs.
    pub fn is_named(&self) -> bool {
        self.osc().is_some()
    }

    /// Write it, if there is anything to write.
    ///
    /// Errors are dropped on purpose, and flushing is not optional: this is the
    /// last thing a launch does before `execvp` replaces the process, so an
    /// escape left sitting in a `BufWriter` is an escape nobody ever writes —
    /// and a terminal that would not take it is not a launch that failed.
    pub fn write(&self) {
        if let Some(osc) = self.osc() {
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(osc.as_bytes());
            let _ = stderr.flush();
        }
    }
}

/// Whether a value of [`DISABLE_VAR`] turns the feature off. Set to anything
/// but a [`FALSEY`] spelling, trimmed and case-folded.
///
/// Lossy rather than fallible, for the reason [`TerminalTitle::wanted`] reads
/// bytes: a value that does not decode is a value somebody set, and every
/// spelling that leaves the feature on is plain ASCII, so no lossy replacement
/// can turn one of those into something else.
fn switched_off(value: Option<&OsStr>) -> bool {
    match value {
        None => false,
        Some(value) => {
            !FALSEY.contains(&value.to_string_lossy().trim().to_ascii_lowercase().as_str())
        }
    }
}

/// `name` with everything a terminal would read as an instruction taken out, or
/// `None` if that leaves nothing worth writing.
///
/// Defence at the boundary the bytes are formed at, and not sold as more than
/// that: the only name that reaches here is a [`crate::launch::Launch::key`],
/// built from a repo slug and a ticket number, and a GitHub repo name cannot
/// hold a control character. What the filter buys is that the guarantee is
/// local — true of this function rather than borrowed from what the tracker
/// happens to allow — so a name assembled from somewhere new later cannot make
/// this the site of an escape-injected title. Dropped rather than escaped,
/// which leaves the writer with nothing to decide.
fn sanitize(name: &str) -> Option<String> {
    let text: String = name.chars().filter(|ch| !ch.is_control()).collect();
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_launch_writes_one_osc_2_sequence() {
        // The whole escape, pinned: `ESC ] 2 ; text BEL`. OSC 2 is the window
        // title (OSC 0 is title *and* icon, which nothing here wants to
        // touch), and BEL rather than ST because it is the terminator every
        // emulator takes, including the ones that treat `ESC \` as two
        // characters.
        let title = TerminalTitle::named("wayfinder#191", None, true);
        assert_eq!(title.osc(), Some("\x1b]2;wayfinder#191\x07"));
        assert!(title.is_named());
    }

    #[test]
    fn nothing_is_written_when_there_is_no_terminal_to_write_to() {
        // A `wf` whose stderr is a file or a pipe is not in a window worth
        // naming, and the escape would land in the file as bytes nobody
        // decodes.
        let title = TerminalTitle::named("wayfinder#191", None, false);
        assert_eq!(title, TerminalTitle::Off);
        assert!(!title.is_named());
    }

    #[test]
    fn a_value_no_encoding_could_read_still_turns_the_title_off() {
        // The direction the rule is *for*, applied to the one value that is not
        // a string: `WF_NO_TITLE=$'\xff1'`. An opt-out asked for by somebody
        // whose shell put a stray byte in it is still an opt-out, and reading
        // an undecodable value as "unset" would name their terminal anyway and
        // silence their agent against their wish.
        use std::os::unix::ffi::OsStrExt;
        let raw = OsStr::from_bytes(b"\xff1");
        assert_eq!(
            TerminalTitle::named("wayfinder#191", Some(raw), true),
            TerminalTitle::Off
        );
    }

    #[test]
    fn the_disable_variable_is_an_opt_out_and_unrecognised_means_off() {
        // The direction that matters: anything set is "off", because the cost
        // of reading a value nobody anticipated as off is one title, while
        // reading it as on is the feature ignoring somebody who asked it to
        // stop. `WF_NO_TITLE=please` is not a spelling this file knows, and it
        // is not a request to keep titling.
        for set in ["1", "true", "yes", "on", "please", "  1  "] {
            assert_eq!(
                TerminalTitle::named("wayfinder#191", Some(OsStr::new(set)), true),
                TerminalTitle::Off,
                "{set:?} must turn the title off"
            );
        }
        // And the way back on without unsetting anything, which is what a
        // `.bashrc` line that once said `WF_NO_TITLE=1` gets edited into.
        // Empty is here because `WF_NO_TITLE=` is how a shell spells "unset"
        // by accident, and exporting nothing must not disable a feature.
        for unset in ["", "0", "false", "no", "FALSE", " no "] {
            assert!(
                TerminalTitle::named("wayfinder#191", Some(OsStr::new(unset)), true).is_named(),
                "{unset:?} must leave the title on"
            );
        }
    }

    #[test]
    fn a_name_cannot_carry_an_escape_of_its_own() {
        // The boundary guard. A name holding its own OSC would otherwise close
        // this sequence and open another, so what a terminal ended up wearing
        // would be the *second* one — a title chosen by whatever composed the
        // name rather than by this function.
        assert_eq!(
            TerminalTitle::named("ws\x1b]2;pwned\x07x\nrest", None, true).osc(),
            Some("\x1b]2;ws]2;pwnedxrest\x07")
        );
        // Nothing left after the filter is nothing worth writing: an empty OSC
        // 2 blanks the window's name, which is a change nobody asked for.
        assert_eq!(
            TerminalTitle::named("\x1b\x07 \n", None, true),
            TerminalTitle::Off
        );
    }
}
