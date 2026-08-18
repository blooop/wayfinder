#!/bin/sh
# Run a command with no `dl` reachable on PATH, and nothing else taken away.
#
# The point of the `default` pixi environment is that devlaunch is not installed
# in it — but `pixi run` *prepends* its prefix to the inherited PATH rather than
# replacing it, so a developer's own `dl` is still there and the environment
# proves nothing. This is what actually takes it away.
#
# ## Why a shadow directory rather than dropping the entry
#
# The obvious implementation deletes any PATH entry that holds a `dl`. That is
# wrong here, and specifically wrong: the ordinary way to get devlaunch is
# `pixi global install devlaunch`, which puts `dl` in `~/.pixi/bin` — the same
# directory as `gh`, as `wf` itself, and as anything else the developer
# installed that way, up to and including the Rust toolchain. Dropping the
# directory takes all of it. The `wf` task below would then run with no `gh`,
# draw an empty picker, and look like the isolation fallback was broken; a
# `cargo` installed the same way would make this script fail with
# `cargo: not found`, having "proved" nothing about hermeticity.
#
# So each offending directory is replaced by a shadow of itself: a scratch
# directory of symlinks to everything in it *except* `dl`. Nothing but the one
# program disappears.
#
# ## Two shell details that are load-bearing
#
# `set -f` around the split, because `IFS=:; for dir in $PATH` does pathname
# expansion as well as word splitting — a PATH entry containing `[`, `*` or `?`
# that matches a real sibling would be silently rewritten to it, and a `dl`
# inside the real directory would then survive untested.
#
# Not `set -e`: the loop is a sequence of tests that are *expected* to fail for
# most directories, and `set -e` plus a bare `[ ... ] && continue` exits on the
# first one — leaving PATH holding only its leading entries, which surfaces as a
# missing compiler rather than as a bug here. `set -u` is kept, because an unset
# variable would silently produce an empty PATH.
set -u

if [ "$#" -eq 0 ]; then
    echo "without-dl.sh: nothing to run" >&2
    exit 2
fi

shadow_root=$(mktemp -d "${TMPDIR:-/tmp}/without-dl.XXXXXX") || exit 1
# Cleaned up rather than left behind, which is why the command below is run as a
# child instead of `exec`ed: an `exec` replaces this shell and the trap never
# fires, and a symlink farm per invocation is not something to leave in /tmp.
trap 'rm -rf "$shadow_root"' EXIT INT TERM

kept=
shadowed=
n=0

set -f
# An empty PATH entry means the working directory, which is not somewhere a
# toolchain lives and is somewhere a checkout might have a file called `dl`.
# Dropped rather than tested.
IFS=:
for dir in $PATH; do
    [ -n "$dir" ] || continue

    # `-f` as well as `-x`: a *directory* named `dl` is executable by this test
    # and is not a program, so checking only `-x` would shadow a directory of
    # the toolchain for no reason.
    if [ ! -f "$dir/dl" ] || [ ! -x "$dir/dl" ]; then
        kept="${kept:+$kept:}$dir"
        continue
    fi

    n=$((n + 1))
    shadow="$shadow_root/$n"
    mkdir -p "$shadow" || exit 1
    # Globbing back on for exactly this loop, and off again before the outer
    # split resumes. The `set -f` above is there to stop `$PATH` being expanded
    # as a pattern; leaving it on here would stop the three patterns below being
    # expanded *as* patterns, so every shadow came out empty and the directory
    # was effectively deleted after all — which is the bug this rewrite exists
    # to fix, reintroduced one line further down. (The outer `for dir in $PATH`
    # list is already expanded, so toggling inside the body is safe.)
    #
    # The three patterns are the standard idiom for "everything including
    # dotfiles but not `.` or `..`": a hidden executable is unusual in a bin
    # directory, and losing one silently is the same class of bug again.
    set +f
    for entry in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
        [ -e "$entry" ] || continue
        [ "${entry##*/}" = "dl" ] && continue
        ln -s "$entry" "$shadow/" 2>/dev/null
    done
    set -f
    kept="${kept:+$kept:}$shadow"
    shadowed="${shadowed:+$shadowed }$dir/dl"
done
unset IFS
set +f

if [ -n "$shadowed" ]; then
    # Said out loud, because a run that found nothing to remove and a run that
    # removed the only `dl` are the same command with very different meanings,
    # and only one of them tested anything.
    echo "without-dl.sh: hidden from PATH: $shadowed" >&2
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

# Captured and re-raised explicitly rather than left to fall off the end: the
# EXIT trap above runs between the command finishing and this script exiting,
# and "the status survives that" is a thing to state rather than to rely on.
"$@"
status=$?
exit "$status"
