//! The Rust version has one home, and nothing else names one.
//!
//! `rust-toolchain.toml` is what every build reads. rustup honours it directly
//! for a developer's shell, for the devcontainer, and for the GitHub runners —
//! which is why no workflow installs a toolchain by name any more — and
//! `recipe/recipe.yaml`, the one consumer that resolves `rust` from conda-forge
//! rather than rustup, loads it explicitly.
//!
//! What this guards is not a wrong version but a *second* one. A workflow that
//! installs a toolchain by name, or a recipe that goes back to a literal,
//! re-creates the split that let `dtolnay/rust-toolchain@stable` move on its own
//! and take `main` red on a day nobody committed anything (#173). A version can
//! only be wrong in one place if it is only written in one place.
//!
//! Offline by design, like its neighbours in this directory: the seam is the
//! configuration text itself, so no network and no toolchain are involved.

use std::fs;
use std::path::PathBuf;

/// A file in this repository, read by its path relative to the crate root.
fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} ships in this repo: {e}", path.display()))
}

/// Every workflow under `.github/workflows`, as `(file name, contents)`.
///
/// Read from the directory rather than from a list, so a workflow added later
/// is held to the same rule without anyone remembering to add it here.
fn workflows() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut found: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} is part of this repo: {e}", dir.display()))
        .map(|entry| entry.expect("the workflow directory is readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file read from a directory has a name")
                .to_string_lossy()
                .into_owned();
            (
                name,
                fs::read_to_string(&path).expect("the workflow is readable"),
            )
        })
        .collect();
    found.sort();
    assert!(
        found.len() >= 4,
        "expected the workflow directory to hold the CI, live, contract and \
         package workflows; found {found:?}"
    );
    found
}

/// The value of `channel` in `rust-toolchain.toml` — the pin itself.
fn pinned_channel() -> String {
    let toml = repo_file("rust-toolchain.toml");
    let line = toml
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("channel"))
        .expect("rust-toolchain.toml names a channel");
    line.split('=')
        .nth(1)
        .expect("`channel` is an assignment")
        .trim()
        .trim_matches('"')
        .to_string()
}

/// The pin is an exact version, not a moving channel.
///
/// `stable` is the specific thing that broke: it is a valid value here and it
/// would leave every consumer reading this file faithfully and still compiling
/// with whatever shipped that morning.
#[test]
fn the_pin_is_an_exact_version() {
    let channel = pinned_channel();
    for moving in ["stable", "beta", "nightly"] {
        assert!(
            !channel.starts_with(moving),
            "rust-toolchain.toml pins `{channel}`, which moves on its own — \
             name an exact version instead"
        );
    }
    let parts: Vec<&str> = channel.split('.').collect();
    assert!(
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "rust-toolchain.toml should pin a full `major.minor.patch` version, got `{channel}`"
    );
}

/// The components the workflows stopped installing are named here instead.
///
/// `ci.yml` used to pass `components: rustfmt, clippy` to the action it no
/// longer uses. Those two steps still run, so the components have to arrive
/// with the toolchain — and this file is now the only thing that asks for them.
#[test]
fn the_pin_carries_the_components_ci_needs() {
    let toml = repo_file("rust-toolchain.toml");
    let components = toml
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("components"))
        .expect("rust-toolchain.toml lists its components");
    for needed in ["clippy", "rustfmt"] {
        assert!(
            components.contains(needed),
            "`{needed}` has a step of its own in ci.yml, so rust-toolchain.toml \
             has to install it: {components}"
        );
    }
}

/// No workflow installs a toolchain by name.
///
/// The action is listed by name because naming a version is the only way to use
/// it — it selects by `@rev` and has never read `rust-toolchain.toml`. The
/// `rustup` subcommands are the hand-rolled equivalents. `rustup show`, which is
/// what the workflows run now, takes no version and is deliberately absent.
#[test]
fn no_workflow_names_a_toolchain_version() {
    let banned = [
        "rust-toolchain@",
        "rustup toolchain install",
        "rustup default ",
        "rustup override ",
        "rustup update ",
    ];
    for (name, body) in workflows() {
        for pattern in banned {
            assert!(
                !body.contains(pattern),
                ".github/workflows/{name} contains `{pattern}`, which names a \
                 toolchain outside rust-toolchain.toml. rustup reads that file \
                 for any cargo command in the checkout, so the step needs no \
                 version at all — see the note in ci.yml"
            );
        }
    }
}

/// The recipe reads the pin instead of repeating it.
///
/// This is the one build that cannot honour `rust-toolchain.toml` implicitly, so
/// it is the one place the coupling is written down — and the one place a
/// literal would quietly reappear.
#[test]
fn the_recipe_reads_the_pin() {
    let recipe = repo_file("recipe/recipe.yaml");
    let requirement = recipe
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("- rust"))
        .expect("the recipe declares a `rust` build requirement");

    assert!(
        requirement.contains(r#"load_from_file("../rust-toolchain.toml").toolchain.channel"#),
        "the recipe's rust requirement should load the pin rather than repeat \
         it, got: {requirement}"
    );
    assert!(
        requirement.contains("=="),
        "the recipe should pin rust exactly, not name a floor: {requirement}"
    );
    assert!(
        !requirement.bytes().any(|b| b.is_ascii_digit()),
        "a version literal has come back into the recipe's rust requirement: \
         {requirement}"
    );
}

/// `package.yml` passes the flag the recipe's read needs.
///
/// `load_from_file` is experimental. Without `--experimental` rattler-build
/// refuses to render the recipe at all, so the build fails loudly rather than
/// silently resolving some other compiler — but it fails in the one workflow
/// that publishes, which is a late place to find out.
#[test]
fn the_package_workflow_enables_the_recipes_read() {
    let package = repo_file(".github/workflows/package.yml");
    let build_args = package
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("build-args:"))
        .expect("package.yml passes build arguments to rattler-build");
    assert!(
        build_args.contains("--experimental"),
        "recipe.yaml reads the pin with `load_from_file`, which is gated behind \
         `--experimental`: {build_args}"
    );
}
