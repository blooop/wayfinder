//! PROTOTYPE (devlaunch#266): `wf` calling `devlaunch-core` in-process instead
//! of shelling out to the `dl` binary.
//!
//! This module is the experiment, not a proposal to merge. It replicates, in
//! `wf`, exactly what the `dl` binary's `session.rs` + `commands.rs` do around
//! the two calls `wf` makes today (`dl --ls --json` and `dl <ws> rm [--force]`),
//! so the cost of the linked shape can be *seen* rather than estimated.
//!
//! What the experiment surfaces, each marked `FINDING` at the line it bites:
//!
//! 1. Nearly every import below is **binary surface** — items core's own docs
//!    say are "not part of the frozen wf API (#251 §7)". The `api` module alone
//!    cannot wire a listing: `enriched_listing` is exported but its parameter
//!    types (`DlView`, `Sizes`, `MetadataStorage`, a `ClonePathResolver`) and
//!    every error type are not.
//! 2. `ListedWorkspace`'s fields are `pub(crate)`, so the typed rows cannot be
//!    read by a linker; the only public reading is `json_document` — JSON. The
//!    linked build therefore *keeps* wf's parse boundary and deletes only the
//!    subprocess.
//! 3. Core's errors and notices carry no `Display` — "no user-facing English in
//!    core" makes the *binary* the renderer, and a linked wf becomes a second
//!    binary: it inherits the whole diagnostic vocabulary (`ConfigError`,
//!    `MetadataError`, `ListingUnreadable`, `NotRun`, migration reports, cache
//!    notices…) as its own rendering duty, or swallows them (this prototype
//!    counts what it swallows).
//! 4. The linked listing runs dl's **cache migration** and metadata open under
//!    wf's own pid — wf becomes a writer of `metadata.json`, holding the same
//!    locks, running v1→v2 migrations dl would otherwise announce.
//! 5. Core is synchronous; wf is tokio. Production use needs `spawn_blocking`
//!    around every call (the test calls it synchronously).
//! 6. Even linked, the `dl` *binary* stays load-bearing: the launch path execs
//!    it, and `workspace_delete` wants a `Refresh` whose `SelfInvocation` names
//!    a re-runnable program — which for wf is the `dl` on PATH, not itself.

use anyhow::{anyhow, bail, Result};

// The one blessed import. Everything below it is binary surface.
use devlaunch_core::api::{enriched_listing, json_document, CommandContext};

// FINDING 1: binary-surface imports, every one required to make the api calls
// callable. `wf` linking these is exactly what #251 §7 said must not happen.
use devlaunch_core::clients::git::Git;
use devlaunch_core::domain::config;
use devlaunch_core::domain::metadata::MetadataStorage;
use devlaunch_core::domain::xdg;
use devlaunch_core::flows::completion_cache;
use devlaunch_core::flows::lifecycle::{
    self, CloneDirectories, DeleteOutcome, Guarded, Insistence, LifecycleNotice, Refresh,
    SelfInvocation,
};
use devlaunch_core::flows::listing::{DlView, Sizes};
use devlaunch_core::flows::migration;
use devlaunch_core::flows::workspace_clone::WorkspaceCloneManager;
use devlaunch_core::runner::Runner;

use crate::reap::{answered_where_dl_answers, parse_workspaces, Workspace};

/// What the linked listing returned, and what it had to drop on the floor.
#[derive(Debug)]
pub struct LinkedListing {
    pub workspaces: Vec<Workspace>,
    /// Typed events core handed back that `wf` has no renderer for: load
    /// notices, migration reports, cache notices. The `dl` binary prints each
    /// of these (some are the user's only pointer to `dl --reconcile`); a
    /// linked `wf` either re-renders them all or loses them. This prototype
    /// counts them. FINDING 3.
    pub swallowed_notices: usize,
}

/// `dl --ls --json`, in-process: the exact call chain of dl's `render_json`,
/// transplanted from `dl/src/session.rs` + `dl/src/commands.rs`.
///
/// # Errors
///
/// Everything dl's own startup can refuse on — no home directory, an unreadable
/// config or `metadata.json`, a devpod listing that would not parse — each
/// phrased here (FINDING 3: core's errors carry no `Display`).
pub fn workspaces_linked(runner: &dyn Runner) -> Result<LinkedListing> {
    let mut context = CommandContext::new(runner);

    // --- dl session.rs, replicated -----------------------------------------
    // FINDING 3 at every `?`: none of these errors impl Display/Error, so they
    // cannot ride anyhow; wf has to phrase each one itself.
    let cache = xdg::devlaunch_cache().map_err(|e| anyhow!("no home directory: {e:?}"))?;
    let config =
        config::worktree_config().map_err(|e| anyhow!("dl config unreadable: {e:?}"))?;
    let path = MetadataStorage::default_path()
        .map_err(|e| anyhow!("no home directory: {e:?}"))?;
    let (mut storage, load_notices) =
        MetadataStorage::open(path).map_err(|e| anyhow!("metadata unreadable: {e:?}"))?;
    // FINDING 4: wf just became the process that migrates dl's cache — and the
    // report (orphaned containers, the pointer to `dl --reconcile`) has no
    // renderer here.
    let mut swallowed = load_notices.len();
    match migration::migrate_cache(&mut storage, &config.repos_dir) {
        Ok(Some(_report)) => swallowed += 1,
        Ok(None) => {}
        Err(_refused) => swallowed += 1,
    }
    let clones = WorkspaceCloneManager::from_config(&config, Git::new(runner));

    // --- dl commands.rs render_json, replicated ----------------------------
    let directories = CloneDirectories::of(&clones);
    let view = DlView {
        cache_dir: &cache,
        storage: &storage,
        clones: &directories,
    };
    let rows = enriched_listing(&mut context, &view, Sizes::Skip)
        .map_err(|e| anyhow!("devpod listing unreadable: {e:?}"))?;
    swallowed += directories.take_notices().len();

    // FINDING 2: the typed rows are opaque; the only public reading is JSON.
    // So the linked build serializes and re-parses through wf's existing parse
    // boundary — the subprocess is gone, the parse is not.
    let body = serde_json::to_vec(&json_document(&rows))?;
    let mut workspaces = parse_workspaces(&body)?;
    // A linked core is definitionally a dl that answers `unsaved` — the version
    // probe's *question* disappears, which is the one genuine simplification.
    answered_where_dl_answers(&mut workspaces, true);
    Ok(LinkedListing {
        workspaces,
        swallowed_notices: swallowed,
    })
}

/// How the linked removal ended, for the caller to phrase.
#[derive(Debug)]
pub enum LinkedRemoval {
    Removed,
    /// dl's own guard said no; the reason is core's typed refusal, Debug-formatted
    /// because rendering it properly is the binary's job wf would be taking over.
    Refused(String),
}

/// `dl <ws> rm [--force]`, in-process: dl's `render_remove`, transplanted —
/// minus target resolution (wf already holds the exact workspace id).
///
/// # Errors
///
/// The same startup refusals as [`workspaces_linked`], a devpod that could not
/// be run, or a devpod that refused the delete.
pub fn remove_linked(runner: &dyn Runner, id: &str, insist: bool) -> Result<LinkedRemoval> {
    let mut context = CommandContext::new(runner);
    let cache = xdg::devlaunch_cache().map_err(|e| anyhow!("no home directory: {e:?}"))?;
    let config =
        config::worktree_config().map_err(|e| anyhow!("dl config unreadable: {e:?}"))?;
    let path = MetadataStorage::default_path()
        .map_err(|e| anyhow!("no home directory: {e:?}"))?;
    let (mut storage, _notices) =
        MetadataStorage::open(path).map_err(|e| anyhow!("metadata unreadable: {e:?}"))?;
    let _ = migration::migrate_cache(&mut storage, &config.repos_dir);
    let clones = WorkspaceCloneManager::from_config(&config, Git::new(runner));

    let insistence = if insist {
        Insistence::Insisted
    } else {
        Insistence::NotInsisted
    };
    let mut notices: Vec<LifecycleNotice> = Vec::new();
    if let Insistence::NotInsisted = insistence {
        let unsaved = lifecycle::unsaved_work_in(
            &clones,
            &storage,
            &context.git(),
            &cache,
            id,
            &mut notices,
        );
        if let Guarded::Refused(refusal) = lifecycle::guard_removal(id, unsaved, insistence) {
            return Ok(LinkedRemoval::Refused(format!("{refusal:?}")));
        }
    }

    // FINDING 6: the refresh child a delete may spawn re-runs a *program*, and
    // the program a linked wf must name is still the dl binary on PATH —
    // `current_exe()` would name wf, which cannot answer `--update-cache`.
    let updater = SelfInvocation::new("dl");
    let cache_path = completion_cache::cache_path(&cache);
    let mut refresh = Refresh::new(&updater, &cache_path);
    let deleted = lifecycle::workspace_delete(
        &mut context,
        &mut refresh,
        &clones,
        &mut storage,
        id,
        insistence,
        &mut notices,
    )
    .map_err(|e| anyhow!("devpod could not be run: {e:?}"))?;
    match deleted {
        DeleteOutcome::DevpodRefused { exit } => bail!("`dl {id} rm` (linked) failed: {exit:?}"),
        DeleteOutcome::Deleted { .. } => Ok(LinkedRemoval::Removed),
    }
}
