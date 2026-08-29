#!/usr/bin/env bash
# Pin a release to exactly one immutable commit.
#
#   tools/assert_release_tag.sh <tag> <expected-commit-sha>
#
# Asserts that
#   1. the checkout really is at the commit this run claims to build, and
#   2. the tag on the origin still points at that same commit.
#
# (2) is the one that matters. A tag that was deleted and re-pushed somewhere
# else, or an old run re-run from the Actions UI after the tag moved, otherwise
# builds one commit while the release notes, the store listing and the
# provenance record all name another. Re-running against a moved tag fails here
# instead of shipping an artifact whose provenance line is a lie.
set -eo pipefail

tag="$1"
expected="$2"

if [ -z "$tag" ] || [ -z "$expected" ]; then
    echo "usage: tools/assert_release_tag.sh <tag> <expected-commit-sha>" >&2
    exit 2
fi

head_sha="$(git rev-parse HEAD)"
if [ "$head_sha" != "$expected" ]; then
    {
        echo "ERROR: the checkout is not the commit this run claims to build."
        echo "  expected: $expected"
        echo "  HEAD:     $head_sha"
    } >&2
    exit 1
fi

if ! git fetch --force --quiet origin "refs/tags/${tag}"; then
    {
        echo "ERROR: tag '${tag}' does not exist on the origin any more."
        echo "It was deleted after this run started, so there is nothing this"
        echo "release could honestly be named after. Re-tag and push again."
    } >&2
    exit 1
fi

# ^{commit} peels an annotated tag to the commit it points at.
tag_sha="$(git rev-parse --verify "FETCH_HEAD^{commit}")"
if [ "$tag_sha" != "$expected" ]; then
    {
        echo "ERROR: tag '${tag}' has moved since this run started."
        echo "  this run built:      $expected"
        echo "  '${tag}' now points at: $tag_sha"
        echo
        echo "A release tag must name exactly one immutable commit. Either re-run"
        echo "from the tag's current commit, or push a new tag; do not ship a"
        echo "build whose provenance names a commit it does not contain."
    } >&2
    exit 1
fi

echo "Tag '${tag}' points at ${expected}, which is what this run is building."
