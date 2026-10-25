# The devcontainer image for working on `wf` — one container per branch, via
# devpod/devlaunch (`dl blooop/wayfinder@<branch>`).
#
# Deliberately thin. A completely cold build of this crate — empty cargo
# registry, empty target/ — measured **9 seconds** on this machine (release 12s,
# incremental one-file 1s, `cargo test --no-run` 2s). So there is no cargo-chef
# dep-prebake, no sccache, and no shared cache volume in here: every mechanism of
# that sort would cost more in complexity and image size than the 9 seconds it
# could save. If that number ever grows into the minutes, this is the file to
# revisit — see blooop/wayfinder#39 for the measurements.
#
# What it *does* carry on top of its base is a Rust toolchain manager and three
# binaries, for different reasons; a fourth, `pixi`, comes with the base and is
# checked rather than downloaded again.
#
# `gh` and the agent CLI are things `wf` itself shells out to and discovers on
# PATH, so a workspace without them cannot run `wf` at all. They are not here
# for the tests: `cargo test` in here is hermetic and stays green with no
# network, because every test that reaches a real GitHub is `#[ignore]`d with a
# reason (see the Checks section of README.md). Running that gated set inside a
# workspace is a thing you can choose to do — `cargo test -- --ignored` — and
# `gh` being present is what makes the choice available.
#
# `pixi` is here for the other half of the gated set: the devlaunch contract
# lanes (`pixi run suite`, `pixi run -e <env> contract`), which block every
# pull request and which `pixi.toml` exists solely to run. Those lanes are how
# an agent working inside a workspace reproduces the PR gate before pushing,
# and before this pin they were runnable only when the developer's dotfiles
# happened to install a pixi of their own — the same check reproducible on one
# machine and impossible on the next, with nothing saying why.
# `tests/devcontainer_prebuild.rs` holds the pin and the install location.
#
# It used to be the largest download in here (78 MB, taking the tool layer from
# 93 MB to 171 MB, measured 2026-08-22), and the paragraph at the top of this
# file rejects cargo-chef and sccache on exactly that ground. The shared base
# below now ships it in /usr/local/bin, so this file pins the version and
# compares it against what the base carries instead of paying for a second
# copy: a base that moves pixi fails this build instead of changing it quietly.
#
# `zellij` is here for a different reason and it is worth saying so, because
# this comment used to justify it by a test file that no longer exists: `wf`
# stopped launching zellij sessions in 9ae2897 and no code or test in this repo
# names it now. It stays for interactive use — a workspace is somewhere a
# person works, and splitting the terminal inside one is worth carrying a
# single static binary for.
#
# The base is the owner's shared devcontainer image, not a Rust image. It is
# Ubuntu 26.04 with a CA bundle that no longer trusts emSign ECC Root CA - G3
# (CVE-2026-111943), and it already carries pixi, git-lfs, a C toolchain and
# the `vscode` user, so every repo of the owner's opens on the same patched
# system. `mcr.microsoft.com/devcontainers/rust:1-bookworm` was Debian 12 and
# needed its own CA upgrade layer. Rust is not in the base, so rustup is
# installed below — rustup and not pixi, because `rust-toolchain.toml` is the
# one place the compiler is named and rustup is what reads it (see `pixi.toml`
# for why pixi does not build this crate).
FROM ghcr.io/blooop/python_template/devcontainer:latest

# The base image ends as `vscode`; the system-wide installs below need root.
USER root

# Pinned rather than "latest" so an image rebuild is a deliberate version bump
# and not a silent one. `gh` and `zellij` are plain static binaries with no build
# step; the agent CLI arrives through its own installer, which turns out to
# accept a version (`install.sh [stable|latest|VERSION]`) — so it is pinned here
# too rather than left to drift, which matters more now that this image is
# published and shared than it did when every workspace built its own.
#
# It is a floor and not a ceiling: the agent CLI updates itself in place, so a
# long-lived container will run something newer than this. What the pin buys is
# that two workspaces created from the same image *start* from the same version.
ARG GH_VERSION=2.97.0
ARG ZELLIJ_VERSION=0.44.3
ARG PIXI_VERSION=0.83.0
ARG CLAUDE_VERSION=2.1.223

# One layer, and the tarballs are deleted in it: an unpacked tarball left behind
# in an earlier layer stays in the image however thoroughly a later layer removes
# it.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) gh_arch=linux_amd64; musl_arch=x86_64-unknown-linux-musl ;; \
      arm64) gh_arch=linux_arm64; musl_arch=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL -o "$tmp/gh.tar.gz" \
      "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_${gh_arch}.tar.gz"; \
    tar -xzf "$tmp/gh.tar.gz" -C "$tmp"; \
    install -Dm755 "$tmp/gh_${GH_VERSION}_${gh_arch}/bin/gh" /usr/local/bin/gh; \
    curl -fsSL -o "$tmp/zellij.tar.gz" \
      "https://github.com/zellij-org/zellij/releases/download/v${ZELLIJ_VERSION}/zellij-${musl_arch}.tar.gz"; \
    tar -xzf "$tmp/zellij.tar.gz" -C "$tmp"; \
    install -Dm755 "$tmp/zellij" /usr/local/bin/zellij; \
    rm -rf "$tmp"; \
    gh --version; zellij --version

# pixi comes from the base, at /usr/local/bin/pixi — on PATH for every user,
# and not ~/.pixi/bin: the image must not squat on the prefix a developer's own
# dotfiles may populate, and the tests distinguish the two by exactly that path.
# The pin above is compared, not trusted, for the same reason the agent CLI's
# is below: a base that moved pixi would otherwise change the PR gates' tool
# with this build still green. Matched on a word boundary (see the agent CLI
# note for why not a substring).
#
# The base's CA bundle no longer trusts emSign ECC Root CA - G3, whose
# intermediates had their private keys cracked in October 2026
# (CVE-2026-111943); Ubuntu removed it in ca-certificates
# 20260816ubuntu0.26.04.1. The `test` fails the build if a base rebuild ever
# brings the root back, rather than shipping it with the build still green.
RUN set -eux; \
    installed="$(/usr/local/bin/pixi --version)"; \
    printf '%s' "$installed" | grep -qwF "$PIXI_VERSION" \
      || { echo "pinned PIXI_VERSION=$PIXI_VERSION but the base carries: $installed" >&2; exit 1; }; \
    test ! -e /etc/ssl/certs/emSign_ECC_Root_CA_-_G3.pem

# rustup, with no toolchain of its own. `rust-toolchain.toml` names the
# compiler and nothing else may (tests/toolchain_pin.rs), and this build cannot
# read that file — its context is `.devcontainer`, see the workflow. So the
# first cargo command in the checkout installs the pinned toolchain, exactly as
# it does on the GitHub runners. The old Rust base image behaved the same way
# whenever its floating `1` tag and the pin disagreed, which was most of the
# time. System-wide under /usr/local, as that image had it, and owned by
# `vscode` so the toolchain install needs no sudo; not ~/.cargo, for the same
# dotfiles reason as pixi above.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN set -eux; \
    install -d -o vscode -g vscode "$RUSTUP_HOME" "$CARGO_HOME"; \
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      | su vscode -c "RUSTUP_HOME=$RUSTUP_HOME CARGO_HOME=$CARGO_HOME sh -s -- -y --no-modify-path --profile minimal --default-toolchain none"; \
    rustup --version

# The agent CLI installs per-user into ~/.local/bin, so it is installed as the
# user that will run it. `~/.claude` is a bind mount from the host (see
# devcontainer.json) and is deliberately *not* what this writes to — the binary
# lives in the image, the credentials and settings come from the host at runtime.
#
# The pin is an argument to a third-party installer, so it is compared, not
# trusted: `gh` and `zellij` above carry their versions in the download URL and
# a wrong version cannot resolve, but an installer that silently ignored its
# argument would leave this pin buying nothing with the build still green. The
# comparison also backstops the unpiped `curl | bash` — a truncated script that
# installed something else fails here.
#
# Matched on a word boundary rather than as a substring, because a substring
# match accepts a version it was never pinned to: `2.1.2234` contains `2.1.223`,
# as does `12.1.223`. Boundary matching refuses both. It is not fully
# shape-proof — `.` and `-` are non-word characters, so a `2.1.223-beta` would
# still match, and a `v`-prefixed `v2.1.223` would be refused (loudly, the safe
# direction) — but it stays agnostic about the line the version arrives in
# (today: `2.1.223 (Claude Code)`), where parsing a field out would be one more
# format to keep in step with the upstream installer.
USER vscode
RUN set -eux; \
    curl -fsSL https://claude.ai/install.sh | bash -s -- "$CLAUDE_VERSION"; \
    installed="$(/home/vscode/.local/bin/claude --version)"; \
    printf '%s' "$installed" | grep -qwF "$CLAUDE_VERSION" \
      || { echo "pinned CLAUDE_VERSION=$CLAUDE_VERSION but installed: $installed" >&2; exit 1; }
ENV PATH=/home/vscode/.local/bin:$PATH
# Left as `vscode`, which is how the base image ends.
