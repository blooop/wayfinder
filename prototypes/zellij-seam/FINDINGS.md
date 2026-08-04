# zellij launch seam — measured findings (issue #5)

Prototype artifact for [#5 "Prove the zellij launch seam"](https://github.com/blooop/wayfinder/issues/5).
All behavior below was measured by running [`prove.sh`](./prove.sh) headless against a
throwaway **detached** session (`zellij attach --create-background wfproto`), with
`bash -c 'sleep 300'` standing in for `claude /wayfinder <map> <ticket>`.

- **zellij version:** 0.44.3
- **Environment note:** the proving shell was itself inside a zellij session, so every
  "outside" test scrubs `ZELLIJ`, `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID` from the env.
  That scrub matters: a bare `zellij action` silently targets whatever session those
  vars name.

## 1. Spawning from OUTSIDE a session — works

Target a named session with the top-level `--session` flag (not on `action`):

```sh
zellij --session wfproto action new-tab  --name 'wf#5' --cwd /tmp -- bash -c 'sleep 300'
# stdout: 1                      <- created tab ID
zellij --session wfproto action new-pane --name 'wf#6' --cwd /tmp -- bash -c 'sleep 300'
# stdout: terminal_2             <- created pane ID
```

Both accept **command + `--cwd` + `--name`** in one shot. The stdout IDs are stable
handles (`focus-pane-id terminal_2`, `go-to-tab-by-id`, `close-tab-by-id`).
The commands genuinely execute even while the session has no attached client
(verified via `pgrep`). Equivalent targeting via `ZELLIJ_SESSION_NAME=wfproto zellij
action ...` also works — but see the hang caveat in §4.

## 2. Spawning from INSIDE the session — works, lands in the same session

A pane's process inherits `ZELLIJ=0`, `ZELLIJ_SESSION_NAME=wfproto`,
`ZELLIJ_PANE_ID=<n>`. A bare, un-targeted call from that process:

```sh
zellij action new-pane --name 'wf#7-inner' --cwd /tmp -- bash -c 'sleep 300'
# stdout: terminal_4, rc=0
```

landed in the same session, in the **same tab as the launching pane** (confirmed via
`dump-layout`). So when `wf` runs inside zellij it needs no explicit targeting at all.

**Focus caveat:** focus is per-*client* in zellij; with zero attached clients there is
no focus to observe (`current-tab-info` errors with "No active tab found for current
client"). `new-pane`'s own help says the default placement "follows the user's focus"
(there is a `--near-current-pane` opt-out), so with an attached client the new pane is
expected to take focus — i.e. selecting a ticket in `wf` would visibly switch you to
the agent pane. **Not verifiable headless; needs one interactive confirmation.**

## 3. Naming / findability — good

Named `wf#<ticket>` panes/tabs are queryable from outside:

```sh
zellij --session wfproto action query-tab-names   # plain list of tab names
zellij --session wfproto action list-tabs         # TAB_ID  POSITION  NAME
zellij --session wfproto action list-panes        # PANE_ID  TYPE  TITLE  (pane names show as TITLE)
zellij --session wfproto action dump-layout       # full KDL: pane names, cwd, command, tab membership
```

Relocation path: `list-panes` title → pane ID → `action focus-pane-id terminal_N`, or
`action go-to-tab-name 'wf#5'` for tabs (both accepted rc=0 from outside). There is no
focus-pane-by-*name*, so `wf` should either keep the pane ID returned at spawn time or
grep `list-panes`. Duplicate names are allowed (two `wf#5` tabs coexisted happily), so
`wf` must not assume name uniqueness — the returned IDs are the reliable handle.

## 4. No-zellij fallback — messages are clean, exit codes are NOT

| Case | Behavior | Exit code |
|---|---|---|
| No env, no `--session` | stderr: `Please specify the session name to send actions to. ...`; stdout: session list | **0** |
| `--session <nonexistent>` | stderr: `Session 'x' not found. The following sessions are active: ...` | **0** |
| `ZELLIJ_SESSION_NAME=<nonexistent> zellij action ...` | **hangs forever** (appears to wait for the session to exist; killed by `timeout 5` → 124) | — |

Consequences for `wf`:
- **Exit codes are useless** for detecting "no session" — zellij returns 0 on these failures.
- Never rely on `ZELLIJ_SESSION_NAME` pointing at a session that might not exist — it hangs.
- The right check is trivial anyway: `wf` should test `$ZELLIJ` in its own env at startup.
  Set → it's inside a session, use bare `zellij action`. Unset → no session owns the
  terminal, so fall back (exec the agent directly, or create a session). Clean seam.

## 5. Cleanup / zombie risk — livable, one flag required

- Default `new-pane -- cmd`: when the command exits the **pane lingers** with an
  EXITED banner (press Enter to re-run) — dead `wf#N` panes would accumulate.
- `new-pane --close-on-exit -- cmd`: pane **removes itself** when the command exits.
  `wf` should pass `--close-on-exit` (or `-c`) and then genuinely needs **no cleanup**:
  tracker-as-truth is livable.
- `zellij kill-session wfproto` reaped every pane command (no orphaned `sleep`s) — no
  zombie risk even for abandoned sessions.
- **Resurrection gotcha:** a killed session is serialized to disk; `attach
  --create-background <same-name>` later *resurrects* it, stale tabs/panes and all
  (commands come back `start_suspended`). If `wf` ever creates sessions by fixed name,
  it should `zellij delete-session <name>` first or expect stale layout.

## Proposed invocation (for reaction, not decided)

```sh
zellij action new-pane --close-on-exit --name "wf#${ticket}" --cwd "${checkout}" \
  -- claude /wayfinder "${map}" "${ticket}"
```

(inside a session; capture stdout `terminal_N` as the findability handle).

## Open questions for a human

1. **Pane vs tab per ticket?** Both support name+cwd+command identically; tabs give
   `go-to-tab-name` (find-by-name) and a visible tab strip per ticket, panes subdivide
   the current tab. Which matches the intended wf UX?
2. **Fallback when `wf` runs outside any zellij session:** exec the agent in place
   (suspending the TUI), refuse with a message, or auto-create a `wf` session and spawn
   into it? All three are implementable; §4 makes detection trivial.
3. **Focus handoff:** headless evidence says the new pane will take focus for an
   attached client (default "follows user's focus"). Is switch-to-agent-on-launch the
   desired behavior, or should wf pass `--near-current-pane`/spawn without focus? Needs
   one interactive check to confirm the observed behavior.
4. **`--close-on-exit` vs keeping the EXITED pane:** self-cleaning panes mean zero
   zombie management, but also mean a crashed agent's output vanishes. Keep the corpse
   for post-mortem, or trust the tracker/logs?
