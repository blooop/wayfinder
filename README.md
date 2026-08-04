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
