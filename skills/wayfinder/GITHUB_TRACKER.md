# Wayfinding operations — GitHub Issues via `gh`

How this tracker expresses the map, child tickets, claiming, blocking, and the frontier.

## Pin the repo first

Every command below targets `$REPO` **explicitly**. Never let `gh` resolve the target itself: `gh issue` and `gh label` pick their repo from ambient state — the cwd at the moment the command runs, a `gh repo set-default` choice, or the `gh-resolved` entry in `.git/config` — and in a fork they resolve through the repo network to the **parent**. A session that steps into a worktree, a vendored checkout, or a subagent's directory would then file tickets into a different repo than the one it is mapping. Resolve once from the working repo's own `origin`, and pass `--repo` on every call:

```bash
REMOTE=origin   # the remote for the repo being mapped — see Forks if origin points at the parent
REPO=$(git remote get-url "$REMOTE" | sed -E 's#^(ssh://)?(git@github\.com[:/]|https://github\.com/)##; s#\.git$##')
gh repo view "$REPO" --json nameWithOwner,visibility,isFork,parent,hasIssuesEnabled \
  --jq '"\(.nameWithOwner) \(.visibility)\(if .isFork then " fork-of:\(.parent.nameWithOwner)" else "" end)\(if .hasIssuesEnabled then "" else " ISSUES-DISABLED" end)"'
```

`$REMOTE` and `$REPO` are the session's two anchors: `$REPO` on every `gh` call, `$REMOTE` on every `git push`. Neither has a default worth trusting.

Show that line to the human before the first write of a charting session and stop if it isn't the repo they meant — the map body carries the Destination and the whole fog sketch, so it is both the most revealing artifact in the flow and the first one created. If `origin` isn't a GitHub remote, use the local-markdown fallback at the bottom of this file rather than reaching for another repo.

The `visibility` field is why that line includes it: if it reads `PUBLIC`, every issue this session files is world-readable. Call it out and get an explicit go-ahead before the first write — see **Keep the map inside its repo** in [SKILL.md](SKILL.md).

## Forks

A fork is its own repo, and the map belongs to **it** — never to its parent. The parent is usually the more public of the two and always the one you have less claim on, so a write that lands there is both a leak and someone else's tracker filling up with your half-formed questions. Three things pull writes upstream; all three are handled the same way, by being explicit:

- **Ambient `gh` resolution.** `gh issue`/`gh label` with no `--repo` resolve through the repo network to the parent. Passing `--repo "$REPO"` on every call — as every snippet here does — pins them to the fork. Don't run `gh repo set-default`, and ignore any `gh-resolved` entry already in `.git/config`; `--repo` overrides both.
- **Issues disabled on the fork.** GitHub creates forks with Issues **off**, which is what the `ISSUES-DISABLED` marker above catches. Turn them on for the fork — this is a one-time repo setting, so confirm with the human before flipping it:

  ```bash
  gh repo edit "$REPO" --enable-issues
  ```

  If they'd rather not, use the local-markdown fallback at the bottom of this file. Filing on the parent is not the fallback.
- **Pushes to the wrong remote.** A fork clone carries a second remote for the parent (`upstream`, sometimes named `origin` if the fork was added later), and `push.default`/`remote.pushDefault`/an inherited upstream branch can send a branch there. Push by name every time — `git push "$REMOTE" research/<name>` — never a bare `git push`, and never to the parent's remote.

If `origin` is the **parent** — you cloned upstream and added your fork as a separate remote — then `origin` is not the repo being mapped. Set `REMOTE` to the fork's remote name before deriving `$REPO`, and the rest follows unchanged.

**Two kinds of id.** `gh issue` subcommands take the issue **number** (`#42`). The sub-issue and dependency REST endpoints take the numeric **database id** in the body. Convert on demand:

```bash
gh api "repos/$REPO/issues/NUMBER" --jq .id
```

## Labels (once per repo, idempotent)

```bash
for l in map research prototype grilling task; do
  gh label create "wayfinder:$l" --repo "$REPO" --color 1D76DB 2>/dev/null || true
done
```

## Create the map

```bash
gh issue create --repo "$REPO" --title "<map name>" --label wayfinder:map --body-file map.md
```

Update the body later (Decisions so far, fog graduation) with `gh issue edit MAP_NUMBER --repo "$REPO" --body-file map.md`. Read-modify-write: fetch the current body first (`gh issue view MAP_NUMBER --repo "$REPO" --json body --jq .body`), edit, write back — other sessions may have appended since you loaded it.

## Create a ticket (create, then parent)

Issues need ids before they can reference each other, so this is always two steps:

```bash
gh issue create --repo "$REPO" --title "<ticket name>" --label wayfinder:grilling --body-file ticket.md
TICKET_ID=$(gh api "repos/$REPO/issues/TICKET_NUMBER" --jq .id)
gh api -X POST "repos/$REPO/issues/MAP_NUMBER/sub_issues" -F sub_issue_id="$TICKET_ID"
```

List a map's children: `gh api "repos/$REPO/issues/MAP_NUMBER/sub_issues"`

## Blocking (native dependencies)

To record "ticket B is blocked by ticket A" (`issue_id` is A's **database id**):

```bash
gh api -X POST "repos/$REPO/issues/B_NUMBER/dependencies/blocked_by" -F issue_id="$A_ID"
```

Inspect: `gh api "repos/$REPO/issues/B_NUMBER/dependencies/blocked_by"` (and `/blocking` for the reverse edge). Remove: `gh api -X DELETE "repos/$REPO/issues/B_NUMBER/dependencies/blocked_by/$A_ID"`. GitHub renders these relationships in the issue UI, so the human sees the frontier without opening the map.

## Claim a ticket

```bash
gh issue edit TICKET_NUMBER --repo "$REPO" --add-assignee @me
```

An open, unassigned ticket is unclaimed. Claim **before any work**.

## Frontier query

Open, unblocked, unclaimed children of the map:

```bash
gh api "repos/$REPO/issues/MAP_NUMBER/sub_issues" --paginate \
  --jq '.[] | select(.state == "open" and (.assignees | length == 0)) | .number' |
while read -r n; do
  open_blockers=$(gh api "repos/$REPO/issues/$n/dependencies/blocked_by" \
    --jq '[.[] | select(.state == "open")] | length')
  [ "$open_blockers" -eq 0 ] && echo "$n"
done
```

## Journal on a ticket (breadcrumbs and handoff)

Breadcrumbs and handoffs are plain issue comments — append-only, never edited:

```bash
gh issue comment TICKET_NUMBER --repo "$REPO" --body "**breadcrumb:** <one or two lines>"
gh issue comment TICKET_NUMBER --repo "$REPO" --body-file handoff.md   # headed "### handoff": where we are / open thread / first move on resume
```

Re-entry reads the trail: `gh issue view TICKET_NUMBER --repo "$REPO" --comments` — scan for the **last** `### handoff`, then the breadcrumbs after it (the whole trail if none).

## Resolve a ticket

```bash
gh issue comment TICKET_NUMBER --repo "$REPO" --body-file resolution.md   # the answer lives here
gh issue close TICKET_NUMBER --repo "$REPO"
# then append the context pointer to the map's Decisions so far (see map edit above)
```

## Local-markdown fallback

Only when the repo has no GitHub remote or issues are disabled. Everything lives in-repo and is committed, so collaborators share it through git:

- **Map**: `.wayfinder/map.md` — same body format as the issue map.
- **Tickets**: `.wayfinder/tickets/<slug>.md` — the `## Question` body, plus a metadata header:

  ```markdown
  - Type: grilling | research | prototype | task
  - Status: open | closed
  - Claimed by: <name, or blank>
  - Blocked by: [<ticket name>](<slug>.md), ...
  ```

- **Claiming** = writing your name to `Claimed by` and committing. **Blocking** = the `Blocked by` list (this tracker has no native blocking, so the body convention applies). **Frontier** = open tickets with empty `Claimed by` whose `Blocked by` entries are all `Status: closed`. **Resolution** = an `## Answer` section appended to the ticket, `Status: closed`, and a Decisions-so-far line in the map. **Breadcrumbs/handoff** = lines appended under a `## Journal` section of the ticket, committed as they land.
