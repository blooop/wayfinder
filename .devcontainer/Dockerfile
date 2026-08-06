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
# What it *does* carry is the three binaries `wf` shells out to and discovers on
# PATH — `gh`, `zellij`, and the agent CLI — because `cargo test` here is not
# hermetic: tests/live_fetch.rs drives `gh` against the real tracker and
# tests/live_zellij_launch.rs starts real zellij sessions.
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# Pinned rather than "latest" so an image rebuild is a deliberate version bump
# and not a silent one. Both are plain static binaries with no build step.
ARG GH_VERSION=2.97.0
ARG ZELLIJ_VERSION=0.44.3

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
USER vscode
RUN set -eux; \
    curl -fsSL https://claude.ai/install.sh | bash; \
    /home/vscode/.local/bin/claude --version
ENV PATH=/home/vscode/.local/bin:$PATH
USER root
