# wayfinder
wf: a multi-project wayfinder manager TUI — fuzzy-find picker and terminal starmap over agentic planning maps

## Discovery

Projects accrete: running `wf` inside a git checkout with a GitHub `origin`
registers that checkout and opens focused on it (`ctrl-g` widens to every
known project, `ctrl-f` re-focuses the row's project). Run outside a checkout
to open on all of them.

The registry is a per-machine cache at `~/.cache/wf/projects.json`
(`$XDG_CACHE_HOME` respected) holding `{path, repo, session}` per checkout.
Deleting it is safe — projects re-accrete as you open them.

Session names derive from the checkout path: the directory name, or the
*parent* directory name when several checkouts share a directory name
(`~/k1/kinisi_ros` → `k1`, `~/k2/kinisi_ros` → `k2`), falling back to the
home-relative path with `/` → `-` if names still collide.

Only repos with an open `wayfinder:map`-labelled issue show tickets; other
checkouts stay cached but hidden.

## Launching

Picking a ticket creates or focuses a zellij tab named `<repo>#<n>` (e.g.
`wayfinder#16`) in that project's session, with the checkout as its cwd,
running the `/wayfinder` skill on the ticket. The tab is the unit of work and
the unit of supervision: it survives `wf` exiting, it is reachable by normal
zellij navigation, and re-picking a ticket focuses its existing tab instead of
starting a second agent on it.

| key | what it does |
| --- | --- |
| `enter` | HITL: create-or-focus the ticket's tab and take you into it (`claude "/wayfinder <map> <n>"`) |
| `ctrl-a` | AFK: spawn the same tab headless (`claude -p "/wayfinder <map> <n>"`) — no attach, no focus steal |
| `↑`/`↓`, `enter`, `esc` | in the which-checkout picker: pick which project hosts the tab, or cancel |

How `wf` hands over depends on where it is running, decided from its own
`$ZELLIJ` — never from a `zellij action` exit code, which is `0` even on
failure:

- **outside zellij** — the TUI suspends, `zellij attach <session>` runs as a
  *child*, and detaching returns to `wf` (which refetches, since the tracker
  moved while you were away);
- **inside the project's own session** — the tab is focused and `wf` keeps
  running in its own tab;
- **inside another session** — zellij's session switcher gesture
  (`switch-session`) moves you over.

No new navigation keybindings: getting back is zellij's standard detach or
tab/session switching. The project's session is created detached if it does not
exist yet (rooted at the checkout; an EXITED session of that name is deleted
first so a stale layout cannot be resurrected).

A finished or crashed agent's tab lingers with an EXITED banner — that is the
post-mortem, and closing it is yours to do. The line above the match count
counts the `<repo>#<n>` tabs zellij is holding, as of the last launch or
`ctrl-r`; it stays empty when there are none.
