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
# What it *does* carry is three binaries, for two different reasons.
#
# `gh` and the agent CLI are things `wf` itself shells out to and discovers on
# PATH, so a workspace without them cannot run `wf` at all. They are not here
# for the tests: `cargo test` in here is hermetic and stays green with no
# network, because every test that reaches a real GitHub is `#[ignore]`d with a
# reason (see the Checks section of README.md). Running that gated set inside a
# workspace is a thing you can choose to do — `cargo test -- --ignored` — and
# `gh` being present is what makes the choice available.
#
# `zellij` is here for a different reason and it is worth saying so, because
# this comment used to justify it by a test file that no longer exists: `wf`
# stopped launching zellij sessions in 9ae2897 and no code or test in this repo
# names it now. It stays for interactive use — a workspace is somewhere a
# person works, and splitting the terminal inside one is worth carrying a
# single static binary for.
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

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
ARG CLAUDE_VERSION=2.1.223

# One layer, and the tarballs are deleted in it: an unpacked tarball left behind
# in an earlier layer stays in the image however thoroughly a later layer removes
# it.
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
      amd64) gh_arch=linux_amd64; zj_arch=x86_64-unknown-linux-musl ;; \
      arm64) gh_arch=linux_arm64; zj_arch=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL -o "$tmp/gh.tar.gz" \
      "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_${gh_arch}.tar.gz"; \
    tar -xzf "$tmp/gh.tar.gz" -C "$tmp"; \
    install -Dm755 "$tmp/gh_${GH_VERSION}_${gh_arch}/bin/gh" /usr/local/bin/gh; \
    curl -fsSL -o "$tmp/zellij.tar.gz" \
      "https://github.com/zellij-org/zellij/releases/download/v${ZELLIJ_VERSION}/zellij-${zj_arch}.tar.gz"; \
    tar -xzf "$tmp/zellij.tar.gz" -C "$tmp"; \
    install -Dm755 "$tmp/zellij" /usr/local/bin/zellij; \
    rm -rf "$tmp"; \
    gh --version; zellij --version

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
USER root
