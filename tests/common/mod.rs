//! Shared fixture for the live integration tests.
//!
//! **Never name a map by number.** Every live test here used to pin
//! `blooop/wayfinder` map `#1`, which quietly assumed that a map reaching its
//! destination would never happen — and when #1 was closed, six tests across
//! three files failed at once, none of them for a reason connected to what
//! they test. `fetch_map` refusing a closed map is *correct* (#28: a moved or
//! finished map must render nothing rather than the wrong issue), so the
//! failure was the fixture's fault, not the code's.

use std::path::Path;

use wf::model::MapId;

pub const THIS_REPO: &str = "blooop/wayfinder";

/// This repo's lowest-numbered open map, looked up live.
///
/// Lowest-numbered rather than arbitrary so a run is reproducible against a
/// tracker that is not moving, and looked up rather than named so the fixture
/// survives any individual map being finished. The only precondition left is
/// that the project has *a* map at all, which is the weakest one a test of the
/// map machinery can have.
pub async fn a_live_map() -> MapId {
    let maps = wf::fetch::find_maps(&[THIS_REPO.to_string()])
        .await
        .unwrap_or_else(|e| panic!("{}", search_failed(&format!("{e:#}"))));
    maps.into_iter()
        .min_by_key(|id| id.number)
        .expect("blooop/wayfinder must have at least one open wayfinder:map issue")
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
fn search_failed(err: &str) -> String {
    let has_token = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()));
    let in_container = Path::new("/.dockerenv").exists();
    match advice(has_token, in_container) {
        Some(advice) => format!("the map search failed: {err}\n\n{advice}"),
        None => format!("the map search failed: {err}"),
    }
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
