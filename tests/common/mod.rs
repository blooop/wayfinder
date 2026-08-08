//! Shared fixture for the live integration tests.
//!
//! **Never name a map by number.** Every live test here used to pin
//! `blooop/wayfinder` map `#1`, which quietly assumed that a map reaching its
//! destination would never happen — and when #1 was closed, six tests across
//! three files failed at once, none of them for a reason connected to what
//! they test. `fetch_map` refusing a closed map is *correct* (#28: a moved or
//! finished map must render nothing rather than the wrong issue), so the
//! failure was the fixture's fault, not the code's.

// Each including binary uses a different part of this module, and dead-code
// analysis runs per binary — so anything used by only one of them would warn
// under CI's `-D warnings` in the other. The alternative is splitting the
// module by consumer, which is more files than the problem is worth.
#![allow(dead_code)]

use std::path::Path;

use tokio::sync::OnceCell;
use wf::model::MapId;

pub const THIS_REPO: &str = "blooop/wayfinder";

/// Looked up once per test binary. The lookup is a ~2.5 s `gh api
/// search/issues`, and four tests in `live_streaming_startup` want the same
/// answer: without this they issue four concurrent searches at t=0, which is
/// enough contention to push the warm-start timing assertion in that file past
/// its budget about half the time. One search per binary, and the tests are
/// measuring what they claim to measure again.
static LIVE_MAP: OnceCell<MapId> = OnceCell::const_new();

/// This repo's lowest-numbered open map, looked up live.
///
/// Lowest-numbered rather than arbitrary because a newly charted map is always
/// the highest-numbered one: picking the lowest means the fixture only moves
/// when a *mature* map is finished, and the next one along has years of
/// tickets on it too.
///
/// What this asks of the tracker, stated rather than implied — the callers
/// assert against whatever comes back, so these are real preconditions:
/// `blooop/wayfinder` has **more than one** open map (`live_discovery`,
/// `live_streaming_startup`), and the lowest-numbered one has at least seven
/// tickets, at least one closed, at least one blocking edge, and no ticket
/// missing its `wayfinder:*` label. Every open map satisfies that today, and a
/// map is charted long before it is worked, so a freshly charted one cannot
/// become the fixture while any older map is still open.
pub async fn a_live_map() -> MapId {
    LIVE_MAP
        .get_or_init(|| async {
            let maps = wf::fetch::find_maps(&[THIS_REPO.to_string()])
                .await
                .unwrap_or_else(|e| panic!("{}", search_failed(&format!("{e:#}"))));
            maps.into_iter()
                .min_by_key(|id| id.number)
                .unwrap_or_else(|| panic!("{}", nothing_found()))
        })
        .await
        .clone()
}

/// The panic message for a failed map search — with the reason it fails for
/// most often spelled out rather than left to be deduced from `gh`'s stderr.
///
/// Raised by the review of #94, which dropped the `~/.config/gh` mount: a
/// container's `gh` login now arrives as `GH_TOKEN`, forwarded by `dl`, so a
/// container brought up any other way (`devpod up` on its own, an editor's
/// devcontainer CLI) has no login at all — and the first thing that says so is
/// a live test failing on a `gh` error several layers down. The fix is to say
/// it here, where the failure surfaces, because that is where somebody is
/// actually reading.
///
/// It fires only on failure, never as a precondition check. A token that is
/// absent while everything works is not a problem worth a warning: on the host
/// `gh` reads its own keyring, and `GH_TOKEN` being unset there is the normal
/// case rather than a broken one.
pub fn search_failed(err: &str) -> String {
    match advice(has_token(), in_container()) {
        Some(advice) => format!("the map search failed: {err}\n\n{advice}"),
        None => format!("the map search failed: {err}"),
    }
}

/// The panic message for a search that *succeeded* and found nothing.
///
/// Separate from [`search_failed`] because the cause is different and so is
/// the fix: `find_maps` returns an empty set rather than an error when the
/// token is valid but cannot see this repo — a fine-grained PAT without access
/// to it, or a `GITHUB_TOKEN` scoped to another repository. Blaming the
/// tracker for having no maps would send somebody looking in exactly the wrong
/// place.
fn nothing_found() -> String {
    let mut msg = format!("the map search found no open `wayfinder:map` issue on {THIS_REPO}");
    if has_token() {
        msg.push_str(
            ". A token is set, so if the tracker does have open maps this is most likely \
             a token that cannot see this repository — a fine-grained PAT without access \
             to it, or a GITHUB_TOKEN scoped elsewhere.",
        );
    }
    msg
}

fn has_token() -> bool {
    ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
}

/// Whether this is running inside a container, by the marker its runtime
/// leaves. Docker writes `/.dockerenv`; podman and anything else built on
/// libcontainer write `/run/.containerenv`. Checking only the first would give
/// a podman workspace the *host* advice, which is the one piece of advice that
/// is actively wrong there — a container has no keyring for `gh` to read.
fn in_container() -> bool {
    Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
}

/// What to tell somebody about a failed search, given the two facts that
/// change the answer. Split out from the environment it reads so the advice
/// can be asserted rather than assumed — a diagnostic nobody has seen render
/// is exactly the kind that turns out to say the wrong thing on the one day it
/// fires.
fn advice(has_token: bool, in_container: bool) -> Option<&'static str> {
    match (has_token, in_container) {
        // A token is present, so it is not the cause; `gh`'s own error is the
        // best thing available and adding to it would only bury it.
        (true, _) => None,
        // Inside a container the missing token *is* the story.
        (false, true) => Some(
            "GH_TOKEN is not set, and this is a container: `gh` has no login here. \
             `dl` forwards the host's token into every workspace it starts, so bring \
             this container up with `dl <checkout>` (see the Isolation section of \
             README.md), or export GH_TOKEN yourself before running the tests.",
        ),
        // On the host it is one candidate among several, and the usual answer
        // is a keyring `gh` can read perfectly well — so point at the check,
        // not at a variable that is normally and correctly unset.
        (false, false) => Some(
            "Neither GH_TOKEN nor GITHUB_TOKEN is set. On a host that is normal — \
             `gh` reads its own keyring — so check `gh auth status` first; these \
             tests need an authenticated `gh` and network.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::advice;

    #[test]
    fn a_token_that_exists_is_not_blamed() {
        assert_eq!(advice(true, true), None);
        assert_eq!(advice(true, false), None);
    }

    #[test]
    fn a_tokenless_container_is_told_where_the_token_comes_from() {
        let msg = advice(false, true).expect("a tokenless container gets advice");
        assert!(msg.contains("dl"), "{msg}");
    }

    #[test]
    fn a_tokenless_host_is_sent_to_gh_auth_status_instead() {
        let msg = advice(false, false).expect("a tokenless host gets advice");
        assert!(msg.contains("gh auth status"), "{msg}");
        assert!(
            !msg.contains("container"),
            "host advice must not talk about containers: {msg}"
        );
    }
}
