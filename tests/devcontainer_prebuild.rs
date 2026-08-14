//! The devcontainer prebuild contract (#150).
//!
//! Offline by design, like `skill_docs.rs` and for the same reason: the seam is
//! the config text itself. What a workspace boots from, and what the publishing
//! workflow is allowed to write to, are promises made in two files that no
//! compiler reads — so they are asserted here rather than discovered by a cold
//! `devpod up` on somebody's laptop.

use serde_json::Value;

/// The published image every workspace boots from. A literal, on purpose: it is
/// the decision (#150), and the tests below check that both the config and the
/// workflow agree with *it* rather than merely with each other.
const PUBLISHED_IMAGE: &str = "ghcr.io/blooop/wayfinder-devcontainer:latest";

fn repo_file(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{relative} ships in this repo: {e}"))
}

/// `devcontainer.json` is JSONC — the spec allows comments, and this repo's
/// configs are mostly comment. `serde_json` is not, so the comments come out
/// first, string literals respected (a `//` inside a value is data, not a
/// comment).
fn strip_jsonc_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for skipped in chars.by_ref() {
                    if previous == '*' && skipped == '/' {
                        break;
                    }
                    previous = skipped;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// One of this repo's devcontainer configs, parsed.
fn devcontainer(relative: &str) -> Value {
    let text = strip_jsonc_comments(&repo_file(relative));
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("{relative} is valid JSONC once its comments are removed (a trailing comma is the usual culprit): {e}")
    })
}

/// The two configs this repo ships: the default, and the variant `dl` selects
/// by bare name (`--devcontainer local` means
/// `.devcontainer/local/devcontainer.json`).
const DEFAULT_CONFIG: &str = ".devcontainer/devcontainer.json";
const LOCAL_CONFIG: &str = ".devcontainer/local/devcontainer.json";

/// Where a path inside a devcontainer config points, relative to the config's
/// own directory — which is how the spec resolves `dockerfile` and `context`,
/// and the only reason the variant can reach a `Dockerfile` it does not sit
/// beside.
fn resolved_from(config: &str, path: &str) -> std::path::PathBuf {
    let config_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/"))
        .join(config)
        .parent()
        .expect("a config path always has a parent directory")
        .to_path_buf();
    config_dir
        .join(path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{config}'s `{path}` names something that exists: {e}"))
}

fn repo_path(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(relative)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{relative} exists in this repo: {e}"))
}

/// The default path — `dl blooop/wayfinder@<branch>` with no variant named —
/// pulls the published image instead of building one. That is the whole point of
/// the prebuild: a cold host boots without compiling a Dockerfile.
#[test]
fn the_default_config_boots_from_the_published_image() {
    let config = devcontainer(DEFAULT_CONFIG);
    assert_eq!(
        config["image"].as_str(),
        Some(PUBLISHED_IMAGE),
        "the default config boots from the published image"
    );
    assert!(
        config.get("build").is_none(),
        "a config with both `image` and `build` is ambiguous: `build` wins in \
         the devcontainer spec, so leaving it here would silently rebuild and \
         the prebuild would buy nothing"
    );
}

/// Building from source is still available, and it builds *this* repo's one
/// Dockerfile — not a copy that can drift from the one the workflow publishes.
/// Asserted by where the paths resolve rather than how they are spelled, so
/// rewriting them is free and pointing them somewhere else is not.
#[test]
fn the_local_variant_builds_this_repos_dockerfile() {
    let config = devcontainer(LOCAL_CONFIG);
    assert!(
        config.get("image").is_none(),
        "the variant exists to build; an `image` here would override the point of it"
    );
    let build = config
        .get("build")
        .expect("the local variant carries the `build:` block the default gave up");
    assert_eq!(
        resolved_from(
            LOCAL_CONFIG,
            build["dockerfile"]
                .as_str()
                .expect("`dockerfile` names a path")
        ),
        repo_path(".devcontainer/Dockerfile"),
        "one Dockerfile, shared with the published image"
    );
    // The narrow context, and it is load-bearing rather than tidy: devpod hashes
    // the build context and gives up past 5000 files ("failed to compute context
    // hash: exceeded limit of 5000 files"), which `target/` alone blows through.
    // The Dockerfile copies nothing out of the context — it only downloads
    // pinned binaries — so the repo root would buy nothing and cost that.
    assert_eq!(
        resolved_from(
            LOCAL_CONFIG,
            build["context"].as_str().expect("`context` names a path")
        ),
        repo_path(".devcontainer"),
        "the context stays this directory: a repo-root context exceeds devpod's \
         5000-file context hash limit on `target/` alone, and buys nothing"
    );
}

/// Two config files is a standing invitation to drift, and the cost of drift
/// here is not cosmetic: a variant that quietly stopped mounting the agent's
/// credentials, or dropped `CLAUDE_CONFIG_DIR`, would look like a working
/// container and behave like a broken one. So the variant may differ from the
/// default in *how the image is obtained* and in nothing else.
#[test]
fn the_variant_differs_from_the_default_only_in_how_the_image_is_obtained() {
    // `name` earns its place in this list — the two containers should be
    // distinguishable in `docker ps`. `image` and `build` are the difference the
    // variant exists for.
    const MAY_DIFFER: [&str; 3] = ["name", "image", "build"];

    let settings = |config: &str| -> std::collections::BTreeMap<String, Value> {
        devcontainer(config)
            .as_object()
            .expect("a devcontainer config is a JSON object")
            .iter()
            .filter(|(key, _)| !MAY_DIFFER.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    };

    assert_eq!(
        settings(LOCAL_CONFIG),
        settings(DEFAULT_CONFIG),
        "the local variant has drifted from the default. Every setting except \
         {MAY_DIFFER:?} must be identical in both — copy the change across, or \
         if the difference is deliberate, say so here and in both files"
    );
}

/// The publishing workflow, with its full-line comments removed. The prose in
/// this repo's workflows is dense and discusses the very triggers asserted
/// below, so a plain substring check against the raw text would be reading the
/// commentary rather than the configuration.
fn publishing_workflow() -> String {
    repo_file(".github/workflows/devcontainer.yml")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A top-level block of a workflow file — `on:`, `permissions:`, `jobs:` — from
/// its key to the next line that starts in column zero.
fn top_level_block(workflow: &str, key: &str) -> String {
    let start = workflow.find(&format!("\n{key}:")).map_or_else(
        || {
            assert!(
                workflow.starts_with(&format!("{key}:")),
                "the workflow declares `{key}:`"
            );
            0
        },
        |index| index + 1,
    );
    let rest = &workflow[start..];
    let mut block = String::new();
    for (number, line) in rest.lines().enumerate() {
        let starts_new_block =
            number > 0 && !line.starts_with([' ', '\t']) && !line.trim().is_empty();
        if starts_new_block {
            break;
        }
        block.push_str(line);
        block.push('\n');
    }
    block
}

/// The decided trigger set: a push to the default branch that actually touched
/// the devcontainer, plus a handle to pull by hand. Deliberately no
/// pull-request leg and no schedule — this image is rebuilt when its inputs
/// change, and a nightly rebuild of an unchanged Dockerfile would only churn the
/// tag every workspace boots from.
#[test]
fn the_workflow_publishes_on_a_devcontainer_change_to_main_or_by_hand() {
    let triggers = top_level_block(&publishing_workflow(), "on");

    assert!(
        triggers.contains("push:") && triggers.contains("branches: [main]"),
        "publishes on a push to main:\n{triggers}"
    );
    assert!(
        triggers.contains(".devcontainer/**"),
        "filtered on the devcontainer directory, so an unrelated push to main \
         does not rebuild and republish the image:\n{triggers}"
    );
    assert!(
        triggers.contains("workflow_dispatch:"),
        "can be fired by hand — a dropped webhook leaves the tag stale with \
         nothing to say so (see #79):\n{triggers}"
    );
    assert!(
        !triggers.contains("pull_request"),
        "no pull-request leg: it could not push anyway (a fork's token cannot \
         write packages), so it would be a build whose only output is a green \
         check:\n{triggers}"
    );
    assert!(
        !triggers.contains("schedule") && !triggers.contains("cron"),
        "no schedule: the image's inputs are the pinned versions in the \
         Dockerfile, so a timed rebuild republishes identical content:\n{triggers}"
    );
}

/// The narrow-job pattern `package.yml` already uses: the workflow as a whole
/// can only read, and the ability to write a package is granted to the one job
/// that publishes. The distinction is what a compromised step in some later,
/// unrelated job would be able to do — overwrite the image every workspace
/// boots from, or not.
#[test]
fn only_the_publishing_job_can_write_a_package() {
    let workflow = publishing_workflow();
    let workflow_level = top_level_block(&workflow, "permissions");

    assert!(
        workflow_level.contains("contents: read"),
        "the default for the whole workflow is read-only:\n{workflow_level}"
    );
    assert!(
        !workflow_level.contains("packages:"),
        "package write is never a workflow-wide grant — every job added later \
         would inherit it silently:\n{workflow_level}"
    );
    assert_eq!(
        top_level_block(&workflow, "jobs")
            .matches("packages: write")
            .count(),
        1,
        "exactly one job may write the package: the one that pushes it"
    );
}

/// Every reference to the devcontainer package anywhere in the workflow, as
/// `repository:tag` pairs.
fn published_references(workflow: &str) -> std::collections::BTreeSet<String> {
    let (repository, _) = PUBLISHED_IMAGE
        .rsplit_once(':')
        .expect("the published image names a tag");
    workflow
        .split_whitespace()
        .flat_map(|token| token.split(['"', '\'', '=', ',', '(', ')']))
        .filter(|token| token.starts_with(repository))
        .map(str::to_string)
        .collect()
}

/// The one contract that spans both files, and the failure it exists to catch:
/// renaming the registry, the repository or the tag in one place and not the
/// other. That mistake publishes an image nothing boots and leaves every
/// workspace pulling a tag nothing publishes — and both halves look correct on
/// their own.
#[test]
fn the_workflow_publishes_exactly_the_reference_the_default_config_boots_from() {
    let workflow = publishing_workflow();
    let booted = devcontainer(DEFAULT_CONFIG)["image"]
        .as_str()
        .expect("the default config boots from an image")
        .to_string();

    assert_eq!(
        published_references(&workflow),
        std::collections::BTreeSet::from([booted.clone()]),
        "the workflow must name exactly the reference the default config boots \
         from ({booted}) and no other — one mutable tag, `latest`, per the \
         decision on #150"
    );
    assert!(
        workflow.contains("packages: write"),
        "naming the reference is not publishing it"
    );
}

/// The built-in token, not a long-lived secret. It is scoped to this repository,
/// it expires with the run, and it means publishing keeps working without a
/// credential anybody has to rotate.
#[test]
fn the_push_authenticates_with_the_runs_own_token() {
    let workflow = publishing_workflow();
    assert!(
        workflow.contains("secrets.GITHUB_TOKEN") || workflow.contains("github.token"),
        "the registry login uses the run's own token:\n{workflow}"
    );
}
