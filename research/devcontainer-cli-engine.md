# Devcontainer CLI as wf's provisioning engine

**Ticket:** [#68](https://github.com/blooop/wayfinder/issues/68)
**Date:** 2026-08-06
**CLI version researched:** `@devcontainers/cli` 0.83.0 (all flag/behavior claims verified empirically against a live Docker 29.5.2 daemon unless noted)

**Verdict:** Yes — `devcontainer up` / `devcontainer exec` is a viable provisioning engine for wf. It honors arbitrary `runArgs` (GPU, devices, host network, X11 mounts), is idempotent per workspace folder with a clean label-based container identity, allocates a PTY for interactive `exec` when wf itself has one, and returns machine-readable JSON from `up`. The two gaps to design around: the CLI has **no `stop`/`down` command** (wf must `docker stop`/`rm` via labels itself), and distribution is **npm/Node-only** (no conda-forge/pixi package, no true static binary).

---

## 1. Host access: runArgs, GPU, devices, X11

### runArgs is passed through verbatim (image/Dockerfile configs)

The spec defines `runArgs` as "an array of Docker CLI arguments that should be used when running the container" and marks it **image- or Dockerfile-based configs only** — it does not apply to Docker Compose configs ([containers.dev JSON reference](https://containers.dev/implementors/json_reference/)).

Verified empirically: a config with

```jsonc
"runArgs": ["--label", "wf.test=1", "--hostname", "wf-test"]
```

produced a container with that label and hostname set (`docker inspect` confirmed). So `--gpus all`, `--device=/dev/...`, `--network=host`, `--privileged`, `-v /tmp/.X11-unix:/tmp/.X11-unix` etc. all flow straight into the `docker run` invocation. X11 env can go in `containerEnv` (e.g. `"DISPLAY": "${localEnv:DISPLAY}"` — `localEnv` substitution is spec-supported).

Related first-class properties (spec, image/Dockerfile only unless noted): `mounts` (same syntax as docker `--mount`, works for X11 socket binds), `containerEnv`, `privileged`, `capAdd`, `securityOpt`, `init`.

### GPU: `hostRequirements.gpu` + `--gpu-availability`

- Spec: `hostRequirements.gpu` takes `true` (required), `"optional"` (use if available), or an object (`cores`, `memory`) ([JSON reference](https://containers.dev/implementors/json_reference/#min-host-reqs)).
- CLI: `devcontainer up --gpu-availability <all|detect|none>` (default `detect`).
- From the CLI source (v0.83.0 bundle): under `detect` it runs `docker info -f '{{.Runtimes.nvidia}}'` and treats a hit on `nvidia-container-runtime` as GPU support; `all` forces true, `none` forces false. When `hostRequirements.gpu` is set and support is found it logs "GPU support found, add GPU flags to docker call." and appends **`--gpus all`** to the `docker run` args. If gpu is required (not `"optional"`) and no support is found, it warns and proceeds.

**Implication for wf:** for GPU repos, either the repo declares `hostRequirements.gpu: "optional"` (portable, auto-detected) or puts `--gpus all` in `runArgs` (fails hard on GPU-less hosts). Note the detection only recognizes the **nvidia runtime** — AMD/other GPUs need explicit `runArgs`/`--device`. wf could pass `--gpu-availability none` as an escape hatch on broken nvidia setups.

### Features

`features` (OCI-distributed setup units, e.g. `ghcr.io/devcontainers/features/nvidia-cuda`) are fully supported by the CLI, including `--additional-features '<json>'` to inject features at `up` time without editing the repo's config — useful if wf ever wants to inject its own tooling layer. Feature versions are pinned via an auto-generated `.devcontainer-lock.json` ([devcontainers/cli README](https://github.com/devcontainers/cli)).

## 2. Lifecycle: idempotency, rebuild, container identity

### Idempotent per workspace folder — verified

Two consecutive `devcontainer up --workspace-folder <dir>` calls returned the **same** `containerId`; the second call reused the running container (and re-ran `postStartCommand`-class hooks only). `up` prints a single JSON line on stdout:

```json
{"outcome":"success","containerId":"9d9c...","remoteUser":"root","remoteWorkspaceFolder":"/workspaces/dcx"}
```

so wf can parse outcome/containerId/remoteWorkspaceFolder directly (logs go to stderr; `--log-format json` available).

### Rebuild

`devcontainer up --remove-existing-container` removes and recreates (verified: new containerId). Companion flags: `--build-no-cache` (image rebuild without docker cache), `--expect-existing-container` (fail rather than create — useful for a wf "attach-only" mode), `--skip-post-create`, `--prebuild`.

### Container identity: labels

The CLI stamps every container it creates with (verified via `docker inspect`):

| Label | Value |
|---|---|
| `devcontainer.local_folder` | absolute host path of the workspace folder |
| `devcontainer.config_file` | absolute host path of the devcontainer.json used |
| `devcontainer.metadata` | JSON array of merged config metadata (also on the image) |

Lookup uses these labels: if `--id-label` is not given, one is **inferred from the workspace folder path** (this is the idempotency key). wf can find a checkout's container without the CLI:

```bash
docker ps -q --filter "label=devcontainer.local_folder=$CHECKOUT"   # verified working
```

Custom `--id-label name=value` (repeatable) replaces the inferred key — wf could stamp `wf.checkout=<id>` for its own identity scheme, but must then pass the same labels to `exec`.

### Gap: no stop/down

v0.83.0 has **no `devcontainer stop` or `devcontainer down`** subcommand (top-level help lists only `up`, `set-up`, `build`, `run-user-commands`, `read-configuration`, `outdated`, `upgrade`, `features`, `templates`, `exec`). Teardown is wf's job: `docker stop`/`docker rm` against the labeled container (respecting the config's `shutdownAction` semantics if wf wants to be spec-faithful).

Worth noting: `--mount-workspace-git-root` defaults to **true** (mounts the git root, not just the subfolder), and there's `--mount-git-worktree-common-dir` for git-worktree checkouts (requires worktrees created with `git worktree add --relative-paths`) — directly relevant if wf provisions per-worktree checkouts.

## 3. Exec model: TTY, cwd, user, env — verified

`devcontainer exec --workspace-folder <dir> <cmd> [args...]` (or `--container-id` / `--id-label`):

- **TTY:** stdio is passed as `inherit` when the corresponding local fd is a TTY, `pipe` otherwise, and stdin raw mode is enabled for interactive sessions (CLI source). Verified: with no local TTY, `tty` in the container reports "not a tty"; run under a PTY (`script -qec ...`), `tty` reports `/dev/pts/0`. **So an interactive agent CLI gets a real PTY exactly when wf runs `devcontainer exec` attached to a terminal** — no extra flag needed, and conversely piped/scripted use degrades cleanly.
- **Working dir:** the container-side `workspaceFolder` (verified: `pwd` → `/workspaces/dcx`).
- **User:** the effective `remoteUser` (falling back to `containerUser`/image user).
- **Env:** both `containerEnv` (baked into the container) and `remoteEnv` (injected per-exec) are present (verified), plus the user's shell env harvested via `userEnvProbe` (default `loginInteractiveShell` — sources login+interactive rc files, cached in a session data folder). Per-invocation extras: `--remote-env NAME=value` (repeatable).
- Exit code of the inner command is propagated; command stdout stays clean (CLI logs go to stderr).

One caveat for wf's TUI: because TTY allocation follows wf's own stdio, launching an agent from inside a TUI means wf must hand its real PTY (or allocate one) to the `devcontainer exec` child — same constraint as any `docker exec -it` wrapper.

## 4. Distribution and what wf should require

Options, in practice ([devcontainers/cli README](https://github.com/devcontainers/cli#try-it-out)):

1. **npm:** `npm install -g @devcontainers/cli` — the canonical channel. Pure-JS install in practice (no native build was needed on this machine), though the README warns some dependencies may want Python/C toolchains.
2. **Standalone install script:** `curl -fsSL https://raw.githubusercontent.com/devcontainers/cli/main/scripts/install.sh | sh` — verified to exist; it is **not a static binary**: it downloads a private Node.js runtime from nodejs.org plus the npm tarball into `~/.devcontainers/` (versioned dirs + `~/.devcontainers/bin/devcontainer` wrapper). Linux/macOS, x64/arm64 only; Windows users are told to use npm/WSL.
3. **conda/pixi:** **no package exists** — `conda-forge/devcontainer-cli` and `conda-forge/devcontainer` both 404 on anaconda.org (checked 2026-08-06). The pixi-native route is: `nodejs` from conda-forge + `npm i -g @devcontainers/cli` (that is exactly how the CLI is installed on the machine this research ran on: `~/.pixi/envs/nodejs/bin/devcontainer`).
4. VS Code's bundled CLI (`code ... devcontainer`) exists but is not scriptable-stable; ignore for wf.

**Recommendation for wf:** require a `devcontainer` executable on PATH (plus a working `docker`), version-check with `devcontainer --version` (parseable semver on stdout), and on absence print a short remedy list in preference order: `npm install -g @devcontainers/cli`, the curl install script, or `pixi global install nodejs && npm i -g @devcontainers/cli`. Gate on a minimum version rather than pinning — the CLI is the reference implementation of the [containers.dev spec](https://containers.dev/implementors/reference/) and moves with it (e.g. `--mount-git-worktree-common-dir` is recent). Do not vendor it: it's ~50 MB with a Node runtime.

---

## Sources

- devcontainers/cli README — <https://github.com/devcontainers/cli>
- Dev Container metadata / JSON reference (runArgs, mounts, hostRequirements.gpu, remoteUser, userEnvProbe) — <https://containers.dev/implementors/json_reference/>
- Spec reference implementation notes — <https://containers.dev/implementors/reference/>
- Standalone install script — <https://raw.githubusercontent.com/devcontainers/cli/main/scripts/install.sh>
- Empirical verification: `@devcontainers/cli` 0.83.0 (`devcontainer up/exec --help`, live container tests, `dist/spec-node/devContainersSpecCLI.js` bundle inspection for label names, GPU detection, and exec stdio handling), Docker 29.5.2, 2026-08-06.
