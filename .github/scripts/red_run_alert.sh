#!/usr/bin/env bash
# Reconcile the tracking issue for one completed run of a watched workflow
# (#190). The workflow (`red-run-alert.yml`) decides *which* runs count and
# hands this script the conclusion; this script's whole job is the idempotent
# half — one open issue per watched workflow, filed on red, never duplicated,
# closed on the next green of the same class.
#
# A script rather than an inline `run:` block so it can be driven for real:
# `tests/red_run_alert.rs` runs it against a recording `gh` stub and asserts
# the create-vs-comment-vs-close decisions at the boundary they cross.
#
# Reads: WORKFLOW_NAME, CONCLUSION, RUN_URL, REPO — plus GH_TOKEN, which `gh`
# itself consumes.
set -euo pipefail

# The issue's identity. An exact-match title rather than a label, because the
# search below can then be read literally: rename the workflow and the old
# issue simply stops matching, which is the correct outcome — a rename also
# re-points the `workflow_run` subscription (the shape `tests/red_run_alert.rs`
# holds both ends to).
title="Red run: ${WORKFLOW_NAME}"

# Exact match on the title, not a substring search: `gh issue list --search`
# tokenises, and "Red run: live" would find "Red run: live tests, take two".
# The `--jq` filter is the real gh's own (embedded gojq), so no jq needs to be
# installed on the runner.
open_issue="$(gh issue list --repo "${REPO}" --state open --limit 100 \
  --json number,title \
  --jq ".[] | select(.title == \"${title}\") | .number" | head -n 1)"

if [ "${CONCLUSION}" = failure ]; then
  if [ -z "${open_issue}" ]; then
    gh issue create --repo "${REPO}" --title "${title}" \
      --body "A non-blocking run of \`${WORKFLOW_NAME}\` went red: ${RUN_URL}

Nothing blocks on this leg, so this issue is the summons. It is updated (not duplicated) while the leg stays red, and closes itself on the next green run of the same class. Filed by \`.github/workflows/red-run-alert.yml\` (#190)."
  else
    gh issue comment "${open_issue}" --repo "${REPO}" \
      --body "Still red: ${RUN_URL}"
  fi
else
  # Green, and only green: the trigger's `types: [completed]` also delivers
  # cancelled and skipped runs, and those are neither evidence of red nor of
  # green — closing a summons on a cancelled run would withdraw it on no
  # evidence at all.
  if [ "${CONCLUSION}" = success ] && [ -n "${open_issue}" ]; then
    gh issue close "${open_issue}" --repo "${REPO}" \
      --comment "Green again: ${RUN_URL}"
  fi
fi
