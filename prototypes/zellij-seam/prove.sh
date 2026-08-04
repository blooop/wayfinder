#!/usr/bin/env bash
# Prototype for wayfinder#5 — prove the zellij launch seam.
#
# Runs headless against a throwaway DETACHED session (default name: wfproto).
# Safe to re-run; kills its own session at the end. Uses `bash -c 'sleep 300'`
# as the stand-in for `claude /wayfinder <map> <ticket>`.
#
# Verified against zellij 0.44.3.
set -u

SESSION="${WF_PROTO_SESSION:-wfproto}"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# Scrub inherited zellij context so the "outside" tests are honest even when
# this script is itself run from within a zellij session (ZELLIJ_SESSION_NAME
# would otherwise silently retarget `zellij action` at the surrounding session).
OUT() { env -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID "$@"; }
# Target the throwaway session explicitly by name, from outside.
Z()   { OUT zellij --session "$SESSION" "$@"; }

step() { printf '\n=== %s ===\n' "$*"; }

step "zellij version"
zellij --version

step "0. create detached background session '$SESSION'"
# A previously killed session with this name is serialized to disk and would be
# RESURRECTED (stale tabs/panes and all) by attach --create-background. Delete
# any dead copy first so we start clean. (Finding in its own right — see FINDINGS.md.)
OUT zellij delete-session "$SESSION" 2>/dev/null
OUT zellij attach --create-background "$SESSION"
OUT zellij list-sessions -n | grep "^$SESSION " || { echo "FATAL: session not created"; exit 1; }

step "1. OUTSIDE -> named session: new TAB with name + cwd + command"
tab_id=$(Z action new-tab --name 'wf#5' --cwd /tmp -- bash -c 'sleep 300')
echo "created tab id: $tab_id"

step "1. OUTSIDE -> named session: new PANE with name + cwd + command"
pane_id=$(Z action new-pane --name 'wf#6' --cwd /tmp -- bash -c 'sleep 300')
echo "created pane id: $pane_id"

step "1. verify the commands are really running (detached session still executes them)"
sleep 1
pgrep -f 'sleep 300' >/dev/null && echo "sleep 300 processes running: $(pgrep -fc 'sleep 300')" || echo "NOT RUNNING"

step "2. INSIDE: pane whose own process spawns a pane via bare 'zellij action'"
# The pane's process inherits ZELLIJ_SESSION_NAME=$SESSION from the session,
# so an un-targeted 'zellij action new-pane' should land in the same session.
Z action new-pane --name 'inside-launcher' -- bash -c "
  env | grep '^ZELLIJ' > '$SCRATCH/inside-env.txt'
  zellij action new-pane --name 'wf#7-inner' --cwd /tmp -- bash -c 'sleep 300' \
    > '$SCRATCH/inside-result.txt' 2>&1
  echo rc=\$? >> '$SCRATCH/inside-result.txt'
  sleep 300"
sleep 2
echo "-- zellij env the inside process saw:"
cat "$SCRATCH/inside-env.txt"
echo "-- result of the inside spawn (pane id + rc):"
cat "$SCRATCH/inside-result.txt"

step "3. findability: query-tab-names / list-tabs / list-panes"
Z action query-tab-names
Z action list-tabs
Z action list-panes
echo "-- which tab each named pane landed in (dump-layout):"
Z action dump-layout | grep -E 'tab name=|name="wf|name="inside'
echo "-- refocus by name (tab) and by id (pane), from outside:"
Z action go-to-tab-name 'wf#5' && echo "go-to-tab-name: rc=0"
Z action focus-pane-id "$pane_id" && echo "focus-pane-id $pane_id: rc=0"

step "4. no-zellij fallback: what does 'zellij action' do without a session?"
echo "-- a) no session context at all (no env, no --session):"
OUT zellij action query-tab-names >"$SCRATCH/4a.out" 2>"$SCRATCH/4a.err"
echo "   exit code: $?   <-- NOTE: 0 even though it did nothing"
echo "   stderr: $(head -1 "$SCRATCH/4a.err")"
echo "   stdout: (a list of all known sessions)"
echo "-- b) --session pointing at a nonexistent session:"
OUT zellij --session definitely-not-a-session action query-tab-names \
  >"$SCRATCH/4b.out" 2>"$SCRATCH/4b.err"
echo "   exit code: $?   <-- also 0"
echo "   stderr: $(head -1 "$SCRATCH/4b.err")"
echo "-- c) ZELLIJ_SESSION_NAME pointing at a nonexistent session:"
OUT env ZELLIJ_SESSION_NAME=definitely-not-a-session timeout 5 zellij action query-tab-names
rc=$?
echo "   exit code: $rc   <-- 124 = killed by timeout, i.e. IT HANGS FOREVER"

step "5. cleanup: pane whose command exits"
Z action new-pane --name 'exiting-default' -- bash -c 'sleep 2'
Z action new-pane --close-on-exit --name 'exiting-closes' -- bash -c 'sleep 2'
sleep 5
echo "-- panes present after both commands exited:"
Z action list-panes | grep exiting || echo "(neither present)"
echo "   (default pane lingers showing an EXITED banner; --close-on-exit pane is gone)"

step "5b. kill-session reaps every pane command"
OUT zellij kill-session "$SESSION"
sleep 1
pgrep -f 'sleep 300' >/dev/null && echo "LEFTOVER sleep processes!" \
  || echo "no leftover sleep processes — kill-session reaped everything"
# Remove the serialized dead session too, so a later run (or user) doesn't resurrect it.
OUT zellij delete-session "$SESSION" 2>/dev/null && echo "serialized session deleted"

step "done"
