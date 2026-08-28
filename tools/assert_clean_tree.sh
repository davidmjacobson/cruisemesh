#!/usr/bin/env bash
# Fail when the working tree does not match the commit the release claims to be.
#
# A release job archives, signs and ships whatever is on disk, while the
# provenance record it writes claims it shipped $GITHUB_SHA. Those are the same
# thing only if the tree is clean at the moment the artifact is produced, so run
# this immediately before the step that produces one.
#
#   tools/assert_clean_tree.sh [allowed-path-prefix ...]
#
# Build outputs that legitimately land inside the repo are passed as allowed
# path prefixes (e.g. ios/CruiseMesh/Frameworks/). Anything else fails the job
# and is printed: a tracked file modified in place -- a generated binding that
# was committed stale and got regenerated, a version stamp applied too early --
# or a stray untracked file that would be compiled in.
#
# Tracked files are compared with --ignore-all-space, matching the gate in
# rust.yml: the checked-in UniFFI bindings are the generator's output with
# trailing whitespace stripped, so a byte-exact comparison would call every
# release dirty. Signature and checksum drift, the thing that actually ships a
# different app, is not whitespace and still fails here.
set -eo pipefail

allowed_count=$#
allowed=()
while [ $# -gt 0 ]; do
    allowed+=("$1")
    shift
done

is_allowed() {
    path="$1"
    i=0
    while [ "$i" -lt "$allowed_count" ]; do
        prefix="${allowed[$i]}"
        case "$path" in
            "$prefix"*) return 0 ;;
        esac
        i=$((i + 1))
    done
    return 1
}

head_sha="$(git rev-parse HEAD)"

offenders=""
offender_count=0
note() {
    offenders="${offenders}  $1"$'\n'
    offender_count=$((offender_count + 1))
}

# Tracked files that differ from HEAD.
while IFS=$'\t' read -r change path rest; do
    [ -n "$path" ] || continue
    # Renames/copies report the destination in the trailing field.
    [ -n "$rest" ] && path="$rest"
    is_allowed "$path" && continue
    note "modified ($change) $path"
done < <(git diff --name-status --ignore-all-space HEAD)

# Untracked, non-ignored files that a build would happily pick up.
while IFS= read -r path; do
    [ -n "$path" ] || continue
    is_allowed "$path" && continue
    note "untracked      $path"
done < <(git ls-files --others --exclude-standard)

if [ "$offender_count" -gt 0 ]; then
    {
        echo "ERROR: the working tree is dirty, so this build is not the commit it"
        echo "claims to be. A release must ship exactly ${head_sha}."
        echo
        echo "Offending paths:"
        printf '%s' "$offenders"
        if [ "$allowed_count" -gt 0 ]; then
            echo
            echo "Expected build outputs (allowed for this step): ${allowed[*]}"
        fi
        echo
        echo "Commit or revert these before tagging a release."
    } >&2
    exit 1
fi

echo "Working tree is clean at ${head_sha}."
