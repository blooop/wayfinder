//! PROTOTYPE (devlaunch#266): drive the linked listing/removal end-to-end
//! against a fake devpod, in a scratch XDG world, and check the rows come out
//! exactly as wf's wire parse would have produced them.

use devlaunch_test_support::FakeRunner;
use wf::linked::{remove_linked, workspaces_linked, LinkedRemoval};
use wf::reap::Unsaved;

/// One test function on purpose: the linked core reads HOME/XDG_* from the
/// process environment at call time, so the scratch world is process state and
/// concurrent tests would race it.
#[test]
fn linked_listing_and_removal_against_a_fake_devpod() {
    // --- a scratch world: never the real cache -----------------------------
    let scratch = tempdir();
    std::env::set_var("XDG_CACHE_HOME", scratch.join("cache"));
    std::env::set_var("XDG_CONFIG_HOME", scratch.join("config"));
    std::env::set_var("HOME", &scratch);

    // --- a machine with two workspaces dl did not make ----------------------
    let fake = FakeRunner::new()
        .with_running("wayfinder-121")
        .with_stopped("devlaunch-266");

    let listing = workspaces_linked(&fake).expect("the linked listing answers");
    assert_eq!(listing.workspaces.len(), 2, "{:?}", listing.workspaces);

    let running = listing
        .workspaces
        .iter()
        .find(|w| w.id == "wayfinder-121")
        .expect("the running row is listed");
    assert!(running.is_running());
    // Not dl's clones, so not wf's to touch — same reading the wire gives.
    assert!(!running.devlaunch);
    assert_eq!(running.unsaved, None);

    let stopped = listing
        .workspaces
        .iter()
        .find(|w| w.id == "devlaunch-266")
        .expect("the stopped row is listed");
    assert!(stopped.is_down());

    // FINDING (see src/linked.rs): what dl would have printed, wf swallowed.
    // On a fresh cache this is at least the metadata-load notices.
    println!("swallowed notices: {}", listing.swallowed_notices);

    // --- the removal guard, in-process --------------------------------------
    // A workspace devpod knows but dl has no record of: the unsaved probe finds
    // no clone, the guard's answer is core's — not a wire parse of stderr.
    match remove_linked(&fake, "devlaunch-266", false) {
        Ok(LinkedRemoval::Removed) => {
            // dl's semantics: no clone of dl's own means nothing to lose, and
            // the devpod delete proceeds.
            assert!(
                fake.args_to("devpod")
                    .iter()
                    .any(|argv| argv.first().map(String::as_str) == Some("delete")),
                "the fake devpod saw the delete"
            );
        }
        Ok(LinkedRemoval::Refused(reason)) => {
            panic!("guard refused a recordless workspace: {reason}")
        }
        Err(error) => panic!("linked removal failed: {error:#}"),
    }

    // The typed Unsaved enum wf keeps is unchanged — the wire arms exist for
    // version skew a linked build cannot have.
    let _ = Unsaved::NothingToLose;
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wf-linkexp-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}
