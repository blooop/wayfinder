# Harness design: what the prior art says to build on top of devlaunch

Synthesis of `prior-art-01-containerised-harnesses.md` (execution harnesses, anti-tamper)
and `prior-art-02-evaluator-agents.md` (LLM judges, review pipelines), mapped onto the
stack already on this machine: `dl` 0.52 (workspaces), `herdr` 0.9 (panes and agent
state), `wf` 0.21 (ticket selection, launch, reap), `prwatch` (PR dispatch). Written
2026-09-17. Claims here are pointers into the two research files, which carry the sources.

## The one-paragraph version

Every open-source harness that keeps a grader out of the worker's reach does it the same
way: the *only* thing that crosses from worker to grader is a diff, and the grader runs in
a fresh container built from a canonical image the worker never touched (01 §8.1-2). METR's
MirrorCode goes to three containers — agent, reference, fresh scorer — after agents defeated
hidden tests by instrumenting the scorer (01 §8.3). On the judging side, the evidence
supports an LLM evaluator only when it is **grounded in evidence the worker could not
author**, given a **reference the worker did not have**, shown the **artifact rather than
the worker's story**, run **read-only**, and wired to **select or retry** — never to accept
(02 Design implications, honest summary). Model independence between worker and judge is a
cheap hedge, not a requirement (02 §1-2). Nobody in production lets a model verdict gate a
merge; the one hard gate that works is GitHub's permission model (02 §19-20).

## The stages, in dl terms

`dl`'s unit of isolation is `(owner, repo, branch)` → one clone, one container. That makes
**the branch the patch-only handoff**: the worker's pushed commits are the diff, and every
later stage is a different branch, hence a different clone and container the worker never
held a handle on.

```
 work            checks                    judge                 verdict
 ─────────────   ────────────────────────   ───────────────────   ─────────────────
 dl devcontainer docker run --rm            claude -p on host     harness process
 on branch W     --network none             read-only tools       on the host
 (reuse ok)      prebuilt image, worktree   worktree base+diff    stores + posts
 herdr pane      base env + filtered diff   reads evidence only   binds on retry
```

**Work.** Worker agent in a `dl` workspace on its branch, in a herdr pane so it is visible
and prompt-able. Reuse along the branch axis is fine here — same work item, same branch,
continue. This is where the earlier "reuse or fresh" question lands: reuse is a *work*-stage
policy only.

**Checks.** A git worktree off dl's bare clone at the base tip, with the worker's diff
applied **filtered**: no `.devcontainer/`, `Dockerfile`, `pixi.toml`/`pixi.lock` (unless the
whitelist rebuild applies), CI config, `conftest.py`; test paths either rejected or reset
from base (SWE-bench restore, 01 §8.11; OpenHands path filter, 01 §8.9; Terminal-Bench
tests-absent, 01 §8.12). Mounted into the repo's prebuilt devcontainer image with
`docker run --rm --network none` and the shared `PIXI_CACHE_DIR` bound, so the environment
is *base's* and the worker's `postCreateCommand` never runs. The deterministic checks run
there, and evidence comes back on **two channels** — a junit/XML report *and* the process
exit status — because a single channel is forgeable (SWE-bench #620 forged `PASSED` lines;
`sys.exit(0)` forges an exit code, 01 §8.22, 02 §14). No results at all is a failure, not
a pass (01 §8.23). Always fresh per *retry* — Anthropic saw agents mining git history from
previous trials (01 §8.4). (`dl owner/repo@harness/eval/<run>` would give the same base-env
property through a full devcontainer; it is the fallback for a repo whose tests need the
devcontainer's mounts, and costs ~16s cold plus a clone.)

**Judge.** `claude -p` on the host, no container: a read-only tool allowlist scoped to a
worktree of base+diff, a different `CLAUDE_CONFIG_DIR` for the login hedge. It is given,
by the harness: the filtered diff; the check evidence; the **task spec from the harness**,
not from the branch; the base commit's `CLAUDE.md`/`AGENTS.md`, not the worker's (02 §16).
It is *not* given the worker's transcript or any worker-authored prose except as delimited
data. Rubric comes from the harness with one explicit criterion per call and no memory of
earlier verdicts (Agent-as-a-Judge dropped its memory module for error cascades, 02 §8).
Prompt carries SWE-agent's line: a test in the patch does not make it better (02 §16).
Different model if it costs nothing.

**Verdict.** Lives on the host, in the harness's own store, never inside a container
(Vivaria, 01 §8.19). It **binds on retry** — the worker gets the report and another go, up
to N — or on **selection** among candidates. It does not merge. The harness posts the report
to the PR with its own token; a human approves. The worker's token can push only to its own
branch (GitHub's coding-agent model: cannot mark ready, approve, or merge, 02 §20).

## What this buys over `wf-review` today

`wf-review` already does two-axis review with fresh subagents — but in the same workspace,
the same checkout, with the worker's `CLAUDE.md`, and with a model reading prose the worker
wrote. The research says that is the configuration that fails: self-critique without
external grounding lowers accuracy in every measured cell (02 §5.1), and worker-authored
text is the injection surface (02 §15). The delta here is structural: separate clone,
base-derived environment, deterministic evidence first, judge reads artifacts not
narration, verdict outside the sandbox.

## Where devlaunch falls short, honestly

1. **Env spec is worker-controlled.** Solved for free by the synthetic-branch trick above,
   *as long as the diff is filtered before it reaches the eval container*. The cost: a
   worker that legitimately changes dependencies gets a false failure. This is the
   "restore the build surface too" hole (01 "what nobody does"), and the trade has to be
   made explicitly — see Open decisions.
2. **No per-workspace GitHub token.** dl forwards the host's `gh` login or nothing
   (`DEVLAUNCH_NO_GH_TOKEN=1`). A worker with the host token can push anywhere branch
   protection allows. Possible without a dl change via a saved workspace variable
   (`--herdr-env set GH_TOKEN=<fine-grained, contents:write, this repo>`), unverified.
3. **No egress control.** Nothing in dl or the devcontainer spec cuts network for one
   workspace. Docker `--network none` would break devpod's ssh transport, which is the
   OpenHands dead end (01 §8.27). Needs iptables-in-container or a devcontainer variant with
   an `--internal` network — a real feature, not a flag.
4. **No read-only mount asymmetry.** Nothing like Modal's `read_only=True` volume (01 §8.18).
   Not needed if the judge only ever gets copies.
5. **Cost.** Measured here (`devlaunch/docs/performance.md`): warm attach ~2.2s, cold
   against a prebuilt image ~16s, of which devpod/tools/ssh plumbing is two-thirds. Disk
   is the real cost — 43GB under one repo's workspaces. So **only the worker gets a
   devcontainer.** Checks run as `docker run --rm --network none` from the prebuilt image
   over a git *worktree* (base + filtered diff) with the shared `PIXI_CACHE_DIR` bound —
   no devpod, no ssh, and `--network none` is possible precisely because there is no ssh
   transport to break, which closes gap 3 for the checks stage for free. The judge is
   `claude -p` on the host with a read-only tool allowlist over a worktree of base+diff —
   no container at all. Cheapest gate first: filter → checks → judge; debounce per push;
   N of 2–3; worktrees never clones for anything ephemeral.

## What herdr is for here

The worker's pane is the human's window. Checks and judge are headless (`dl -- cmd`), or in
panes tagged with `herdr pane report-metadata` tokens (`run=<id> stage=work|checks|judge`)
so the agent panel filters by stage — the agent-queue plugin already drives filters off
tokens. herdr is the *display*, not the control plane: control goes through `dl`'s
subprocess contract, which is `exec`-style and so compatible with a future egress cut.

## Relationship to wf and prwatch

The pipeline is a pure function of `(repo, base, branch, task spec) → verdict`. It does not
need wf's map model or prwatch's PR gate. Recommendation: ship it as its own CLI
(`harness run owner/repo@branch --task <spec>` → verdict JSON + PR report) that `wf` calls
after `wf-tdd` and `prwatch` calls after a dispatch — one definition of "passed", audited
once, the same reason `reap.rs` is the one definition of "finished".

## Decisions (2026-09-17)

- **Env changes in the diff: flag and rebuild from a whitelist.** Env-defining files are
  split off the diff. If only `pixi.toml` + `pixi.lock` changed, the eval container is
  rebuilt from the branch's spec and the judge is told so explicitly. Changes to
  `.devcontainer/`, `Dockerfile` or CI config reject the run. A reject is a verdict with
  a reason, not a crash.
- **Verdict binds on retry up to N, then report.** Below threshold the worker gets the
  report and another go, fresh checks and judge workspaces each time. After N, the report
  is posted and a human decides. No merge gate.
- **Home: a new CLI that wf and prwatch both call.** `harness run owner/repo@branch --task
  <spec>` → verdict JSON + PR report. Ships in the pixi metapackage alongside dl and herdr.
  `wf-review`'s in-review stage becomes a caller once the CLI exists.
- Still open, lower stakes: worker token scope (start with the host token, move to a
  fine-grained per-repo token via a saved workspace variable once verified), and judge
  model (start with a different Claude login, same model; swap in a different model if it
  costs nothing).

## Do first, before infrastructure

Try the prompt (01 §8.33: a prompt alone took GPT-5 from 92% to 1% cheating on
ImpossibleBench; Anthropic uses inoculation prompting in production). Then build the
checks stage and measure the infrastructure's delta against best-of-N with a real verifier
(02 §12) — if the judge cannot beat that, it is buying nothing.
