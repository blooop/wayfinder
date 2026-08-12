#!/bin/sh
# Run a command with every directory that holds a `dl` removed from PATH.
#
# The point of the `default` pixi environment is that devlaunch is not
# installed in it — but `pixi run` *prepends* its prefix to the inherited PATH
# rather than replacing it, so a developer's `~/.pixi/bin/dl` is still there and
# the environment proves nothing. This is what actually takes it away.
#
# Removing directories rather than unsetting PATH: the build needs `cargo`,
# `rustc`, `cc` and `git`, none of which live beside a `dl`. Anything that does
# ship a `dl` in the same directory as the compiler was already unable to
# express this test.
#
# Not `set -e`: the loop below is a sequence of tests that are *expected* to
# fail (most directories have no `dl` in them), and `set -e` plus a bare
# `[ ... ] && continue` exits the script on the first such directory — which
# leaves PATH holding only its leading entries and the failure looks like a
# missing compiler. `set -u` is kept, because an unset variable here would
# silently produce an empty PATH.
set -u

if [ "$#" -eq 0 ]; then
    echo "without-dl.sh: nothing to run" >&2
    exit 2
fi

kept=
found=
# An empty PATH entry means the working directory, which is not somewhere a
# toolchain lives and is somewhere a checkout might have a file called `dl`.
# Dropped rather than tested.
IFS=:
for dir in $PATH; do
    [ -n "$dir" ] || continue
    # `-f` as well as `-x`: a *directory* named `dl` is executable by this test
    # and is not a program, so checking only `-x` would throw away a directory
    # of the toolchain for no reason.
    if [ -f "$dir/dl" ] && [ -x "$dir/dl" ]; then
        found="${found:+$found }$dir/dl"
        continue
    fi
    kept="${kept:+$kept:}$dir"
done
unset IFS

if [ -n "$found" ]; then
    # Said out loud, because a run that silently found nothing to remove and a
    # run that removed the only `dl` are the same command with very different
    # meanings, and only one of them tested anything.
    echo "without-dl.sh: removed from PATH: $found" >&2
fi

PATH=$kept
export PATH

# A post-condition on the loop above, not a guard against the outside world:
# with the same PATH the loop just walked, `command -v` should agree there is
# nothing left. It is here because the *failure* mode of a wrong filter is
# silent — the suite goes back to testing whatever the developer has installed
# and still passes — so the loop is the one piece of this script that must not
# be allowed to be subtly wrong after somebody edits it.
if command -v dl >/dev/null 2>&1; then
    echo "without-dl.sh: a \`dl\` is still reachable at $(command -v dl)" >&2
    exit 1
fi

exec "$@"
