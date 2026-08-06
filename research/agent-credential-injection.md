# Injecting the agent and credentials into arbitrary devcontainers

Research for [#69](https://github.com/blooop/wayfinder/issues/69). Question: given the settled
decision to mount from the host, how does wf get the claude agent and its credentials (plus `gh`)
into an arbitrary repo's devcontainer — binary compatibility, config surface, mount mechanics at
`devcontainer up` time, and credential safety.

Evidence: Anthropic's Claude Code docs, the `devcontainer up --help` output and dist bundle of
devcontainers/cli v0.83.0, gh CLI manual, and inspection of a live host install
(claude 2.1.223 native, gh 2.97.0).

---

## 1. Binary compatibility: can the host binary be bind-mounted?

**"npm vs native" is a non-question at runtime — both are the same native binary.**
The npm package no longer ships a Node.js program: it pulls a per-platform native binary through
optional dependencies (`@anthropic-ai/claude-code-linux-x64`, `-linux-x64-musl`, `-linux-arm64`,
`-darwin-arm64`, …) and a postinstall step links it into place; "the installed `claude` binary does
not itself invoke Node" ([setup docs](https://code.claude.com/docs/en/setup#install-with-npm)).
Node is only needed by npm at install time. So the container does **not** need Node for the agent
to run.

**But the native binary is libc-specific.** Verified on a live native install:

```
~/.local/bin/claude -> ~/.local/share/claude/versions/2.1.223
versions/2.1.223: ELF 64-bit LSB executable, x86-64, dynamically linked,
                  interpreter /lib64/ld-linux-x86-64.so.2, for GNU/Linux 3.2.0
```

Consequences for bind-mounting the host binary into arbitrary base images:

- **glibc image (Ubuntu/Debian/Fedora)**: works. The binary targets GNU/Linux 3.2.0, so any
  non-ancient glibc distro runs it.
- **musl image (Alpine)**: fails — `/lib64/ld-linux-x86-64.so.2` doesn't exist, exec reports the
  misleading `not found`. A separate musl build exists (npm's `linux-x64-musl`), and even that
  needs `libgcc libstdc++ ripgrep` installed plus `USE_BUILTIN_RIPGREP=0`
  ([Alpine section](https://code.claude.com/docs/en/setup#alpine-linux-and-musl-based-distributions)).
- **Mount the version tree, not the launcher**: `~/.local/bin/claude` is a symlink into
  `~/.local/share/claude/versions/`; mounting only the launcher yields a dangling symlink. Mount
  `~/.local/share/claude` (plus a launcher path) — and mount it **read-only** with
  `DISABLE_AUTOUPDATER=1`, because the auto-updater writes new versions into that tree, and a
  read-write mount would let repo-controlled container code replace the binary the *host* executes
  (see §4).

**What Anthropic actually documents for containers** is installing *inside* the image, not
mounting the binary: the
[Claude Code Dev Container Feature](https://github.com/anthropics/devcontainer-features/tree/main/src/claude-code)
(`ghcr.io/anthropics/devcontainer-features/claude-code:1.0`) installs the latest CLI into any
devcontainer and installs Node itself if the base image lacks it
([devcontainer docs](https://code.claude.com/docs/en/devcontainer)). Crucially for wf, a feature
can be injected at `up` time via `--additional-features` without touching the repo (§3).

**Verdict**: bind-mounting the host binary is viable as a fast path when the image is glibc
(the overwhelmingly common case for devcontainers), with a `ldd`-style or
`test -e /lib64/ld-linux-x86-64.so.2` probe as a guard; the robust fallback for musl/exotic images
is injecting the Anthropic devcontainer feature (adds image-build time, always correct libc).

## 2. Config surface: what the agent and gh actually need

### claude

| Path | Contents | Access |
|---|---|---|
| `~/.claude/` | `.credentials.json` (OAuth/API credentials on Linux; macOS uses Keychain instead), `settings.json`, `history.jsonl`, `projects/` (session history), `plugins/`, `commands/`, `skills/`, `shell-snapshots/`, caches | **read-write** — session state, history, and locks are written constantly |
| `~/.claude.json` | OAuth account, personal MCP servers, **per-project trust**, onboarding state | **read-write** — rewritten on trust prompts and startup |

Two traps documented by Anthropic
([devcontainer docs](https://code.claude.com/docs/en/devcontainer#persist-authentication-and-settings-across-rebuilds),
[.claude directory](https://code.claude.com/docs/en/claude-directory)):

1. `~/.claude.json` lives *outside* `~/.claude`, so "mounting a volume at `~/.claude` alone doesn't
   keep you signed in." The fix is to set `CLAUDE_CONFIG_DIR` to the mounted directory — Claude
   Code then keeps `.claude.json` (and everything else) inside it. Confirmed on the live host:
   with `CLAUDE_CONFIG_DIR` set, `~/.claude/` contains both `.credentials.json` and `.claude.json`.
2. On macOS hosts the OAuth token is in the Keychain, not in `.credentials.json` — a mounted
   `~/.claude` from a Mac host may carry no usable credential. For a Linux-first tool this is a
   caveat to document, not solve.

Alternative to mounting live host config: a long-lived `CLAUDE_CODE_OAUTH_TOKEN` from
`claude setup-token`, or `ANTHROPIC_API_KEY`, passed as environment — Anthropic's recommended
pattern for Codespaces secrets ([devcontainer docs](https://code.claude.com/docs/en/devcontainer)).

### gh

| Path | Contents | Access |
|---|---|---|
| `~/.config/gh/` (`$GH_CONFIG_DIR`) | `config.yml` (preferences), `hosts.yml` (host, user, `oauth_token` **when keyring is unavailable**) | read-only suffices |
| `~/.local/state/gh/` | runtime state (update-check, device-id) | written at runtime, safe to omit |

The catch: `gh auth login` stores the token "securely in the system credential store" and only
"falls back to writing the token to a plain text file" (`hosts.yml`) when no keyring is available
([gh auth login manual](https://cli.github.com/manual/gh_auth_login)). On a desktop host with a
keyring, `hosts.yml` contains **no token** and mounting `~/.config/gh` yields an unauthenticated
gh inside the container. The robust path is the one gh documents for headless/automation use:
`GH_TOKEN` env var ([gh environment manual](https://cli.github.com/manual/gh_help_environment)),
which wf can obtain on the host with `gh auth token` regardless of where the host stores it.
`GH_TOKEN` takes precedence over any mounted config.

## 3. Mount mechanics at `devcontainer up` time (no repo edits)

From `devcontainer up --help` (devcontainers/cli 0.83.0), the relevant knobs:

- **`--mount type=<bind|volume>,source=<source>,target=<target>[,external=<true|false>]`** —
  repeatable, adds mounts on top of the repo's config. **Limitation confirmed in the CLI source**:
  the flag is parsed into an object and re-emitted to docker as only `type=…,src=…,dst=…`
  (dist bundle: `function gL(e){…if(typeof e=="string")return[A,e]; …t=`type=${e.type},` …}`).
  There is **no way to express `readonly` via `--mount`** — flag-injected bind mounts are
  read-write.
- **String mounts in config pass through verbatim** to `docker --mount` (same function, string
  branch), so `"source=/home/x/.local/share/claude,target=/opt/claude,type=bind,readonly"` in a
  devcontainer.json `mounts` array *does* work — but that requires controlling the config.
- **`--override-config <path>`** — *replaces* the repo's devcontainer.json entirely ("path to
  override any devcontainer.json in the workspace folder"). To use it non-destructively wf would
  have to parse the repo's (JSONC) config and merge its own `mounts`/`containerEnv` in. Powerful
  (this is the only up-time route to read-only mounts, `containerEnv`, and `${localEnv:HOME}`
  expansion) but requires a JSONC merge step; VS Code itself uses this mechanism internally.
- **`--additional-features '<json>'`** — merges extra features into the repo's config, e.g.
  `'{"ghcr.io/anthropics/devcontainer-features/claude-code:1.0":{}}'` installs the agent with no
  mounts and no repo edits. This is the clean fallback when binary-mounting won't fly.
- **`--remote-env NAME=value`** — env for user commands; also available on `devcontainer exec`,
  which is how wf launches the agent, so `CLAUDE_CONFIG_DIR`, `GH_TOKEN`,
  `CLAUDE_CODE_OAUTH_TOKEN`, `DISABLE_AUTOUPDATER` can all ride per-exec without touching config.
- **`--secrets-file <path.json>`** — key-value secret env vars from a file, keeping tokens out of
  argv/process listings and CLI logs. Preferable to `--remote-env` for tokens.
- `${localEnv:VAR}` interpolation works in config values (e.g. `${localEnv:HOME}`) — useful inside
  a wf-generated override config to avoid hardcoding `/home/<user>`; it is *not* available inside
  `--mount` flag values.

**Practical recipe (no repo edits, glibc image):**

```bash
devcontainer up --workspace-folder "$repo" \
  --mount "type=bind,source=$HOME/.local/share/claude,target=/wf/claude-dist" \  # rw, see §4 for ro
  --mount "type=volume,source=wf-claude-state-$id,target=/home/$remoteUser/.claude"
devcontainer exec --workspace-folder "$repo" \
  --remote-env CLAUDE_CONFIG_DIR=/home/$remoteUser/.claude \
  --remote-env DISABLE_AUTOUPDATER=1 \
  --remote-env GH_TOKEN="$(gh auth token)" \
  -- /wf/claude-dist/versions/<ver> …
```

To get the binary mount **read-only** (recommended, §4), the mount must move from `--mount` into a
merged `--override-config` with a string mount ending `,readonly`.

## 4. Safety posture: host credentials vs repo-controlled images

Anthropic states the threat model plainly
([devcontainer docs](https://code.claude.com/docs/en/devcontainer)):

> When executed with `--dangerously-skip-permissions`, dev containers do not prevent a malicious
> project from exfiltrating anything accessible inside the container, including the Claude Code
> credentials stored in `~/.claude`. Only use dev containers when developing with trusted
> repositories… Avoid mounting host secrets such as `~/.ssh` or cloud credential files into the
> container; prefer repository-scoped or short-lived tokens.

Concrete risks for wf's "mount from host" design:

- **Exfiltration**: `postCreateCommand`/`onCreateCommand`/Dockerfile run repo-controlled code as
  the container user *before any human is in the loop*. Any mounted credential
  (`~/.claude/.credentials.json`, `hosts.yml` token) is readable at `up` time. **Read-only mounts
  do not help confidentiality** — they only prevent tampering.
- **Host poisoning via rw mounts** (the sharper, less obvious risk):
  - rw mount of `~/.local/share/claude` → container code can replace the binary the **host** runs
    next time the user types `claude`. Mount the install tree read-only, always.
  - rw mount of the *live* host `~/.claude` → container code can edit `settings.json` (hooks
    execute shell commands in every future session, including host sessions) or plant commands/
    skills. Sharing the live host config dir rw hands the repo persistent code execution on the
    host.
- **Blast-radius asymmetry**: an OAuth credential from a Claude subscription is account-wide; a gh
  keyring token is whatever scopes it was minted with. Tokens minted for the purpose
  (`claude setup-token` → `CLAUDE_CODE_OAUTH_TOKEN`; a fine-grained PAT or `gh auth token` for
  `GH_TOKEN`) are revocable and auditable.

**Sensible posture — two tiers:**

1. **Default (arbitrary/untrusted repos)**: never bind-mount live host credential dirs.
   - Agent state: per-container **named volume** at `~/.claude` + `CLAUDE_CONFIG_DIR` pointing at
     it (exactly Anthropic's reference config, which uses
     `source=claude-code-config-${devcontainerId}`); credentials enter as env via `--secrets-file`
     (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`, `GH_TOKEN`). Worst case = token theft →
     revoke.
   - Binary: read-only mount of the host install tree (glibc images) or
     `--additional-features` claude-code feature (everything else).
2. **Opt-in convenience (trusted repos, explicit flag)**: bind-mount host `~/.claude` rw with
   `CLAUDE_CONFIG_DIR` set (full session continuity with the host), `~/.config/gh` read-only.
   Document that this grants the repo read access to tokens and write access to host agent config.
3. **Never** mount `~/.ssh` or cloud credential files; `--skip-post-create` exists if wf ever
   wants a "cold" inspection mode before running repo lifecycle scripts.

---

## Sources

- Claude Code devcontainer guidance — https://code.claude.com/docs/en/devcontainer
- Claude Code setup (native/npm installers, Alpine/musl, binary layout) — https://code.claude.com/docs/en/setup
- `~/.claude` directory & `CLAUDE_CONFIG_DIR` — https://code.claude.com/docs/en/claude-directory
- Anthropic reference devcontainer — https://github.com/anthropics/claude-code/tree/main/.devcontainer
- Claude Code devcontainer feature — https://github.com/anthropics/devcontainer-features/tree/main/src/claude-code
- devcontainers/cli — `devcontainer up --help` (v0.83.0) and dist bundle mount serialization; repo: https://github.com/devcontainers/cli
- gh credential storage — https://cli.github.com/manual/gh_auth_login
- gh environment variables (`GH_TOKEN`, `GH_CONFIG_DIR`) — https://cli.github.com/manual/gh_help_environment
- Live host inspection: claude 2.1.223 native install (glibc ELF, symlink layout,
  `~/.claude/.credentials.json`), gh 2.97.0 (`hosts.yml` with plaintext `oauth_token` on a
  keyring-less host; state in `~/.local/state/gh`)
