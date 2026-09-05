#!/usr/bin/env bash
# relay_deploy.sh -- rebuild and restart the hosted CruiseMesh relay at the
# commit that is actually checked out, then prove /healthz says so.
#
#   git -C /opt/cruisemesh pull --ff-only origin master
#   /opt/cruisemesh/tools/relay_deploy.sh
#
# Why this exists rather than `docker compose up -d --build`:
#
# `/healthz` reports the commit baked into the image at build time. The build
# context excludes `.git` (relayd/Dockerfile), so the SHA has to be injected,
# and it used to be injected from a static `GIT_SHA=` line in the box's
# `relayd/.env` via `GIT_SHA: ${GIT_SHA:-unknown}` in compose. Pull new code,
# forget to hand-edit `.env`, redeploy -- and the relay ran the new commit
# while telling everyone who asked that it was running the old one. That is
# not a missing answer, it is a confident wrong one, and it sent two real
# deploy investigations chasing changes that were in fact already live.
#
# So the value comes from the invocation, never from a file: this script
# reads HEAD out of the checkout and passes it as a build arg for this build
# only. `relayd/.env` no longer carries GIT_SHA at all, and an image built
# without one refuses to build (see relayd/Dockerfile and relayd/build.rs).
#
# Environment:
#   ALLOW_DIRTY=1  Deploy from a tree with uncommitted tracked changes. The
#                  image is then stamped `<sha>-dirty` so /healthz keeps
#                  saying that the running code is not exactly any commit.
#
# Needs: bash, git, docker with the compose plugin.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
COMPOSE_DIR="$ROOT/relayd"

die() { echo "error: $*" >&2; exit 1; }

command -v git >/dev/null || die "git not found"
command -v docker >/dev/null || die "docker not found"
[ -f "$COMPOSE_DIR/docker-compose.yml" ] || die "no docker-compose.yml under $COMPOSE_DIR"

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 ||
  die "$ROOT is not a git checkout, so there is no commit to report -- deploy from a clone, not an unpacked tarball"

GIT_SHA=$(git -C "$ROOT" rev-parse --short HEAD) ||
  die "could not read HEAD in $ROOT"
[ -n "$GIT_SHA" ] || die "git rev-parse --short HEAD produced nothing"

# Tracked modifications only. The box legitimately carries untracked files
# next to the compose stack -- `relayd/.env`, a private
# docker-compose.override.yml for the APNs key (DEPLOY.md 7.1), backups --
# and none of them change which commit the binary is built from. What must
# not differ is the source that gets compiled.
DIRTY=$(git -C "$ROOT" status --porcelain --untracked-files=no)
if [ -n "$DIRTY" ]; then
  if [ "${ALLOW_DIRTY:-}" = "1" ]; then
    GIT_SHA="${GIT_SHA}-dirty"
    echo "warning: deploying a modified tree; stamping the image ${GIT_SHA}" >&2
    echo "$DIRTY" >&2
  else
    {
      echo "error: uncommitted changes to tracked files, so the build would not be ${GIT_SHA}:"
      echo "$DIRTY"
      echo
      echo "Commit, revert or stash them, or re-run with ALLOW_DIRTY=1 to deploy"
      echo "anyway and have /healthz report ${GIT_SHA}-dirty."
    } >&2
    exit 1
  fi
fi

cd "$COMPOSE_DIR"

echo "==> [1/3] Building relayd at ${GIT_SHA}"
docker compose build --build-arg "GIT_SHA=${GIT_SHA}" relayd

echo "==> [2/3] Starting the stack"
docker compose up -d

echo "==> [3/3] Verifying /healthz reports ${GIT_SHA}"
health=""
for _ in $(seq 1 40); do
  if health=$(docker compose exec -T relayd curl -fsS http://127.0.0.1:8080/healthz 2>/dev/null); then
    break
  fi
  health=""
  sleep 3
done

[ -n "$health" ] ||
  die "relayd never answered /healthz; check 'docker compose logs relayd' in $COMPOSE_DIR"

reported=$(printf '%s' "$health" |
  sed -n 's/.*"commit"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')

if [ "$reported" != "$GIT_SHA" ]; then
  {
    echo "error: the relay reports commit '${reported}', not '${GIT_SHA}'."
    echo "Response: $health"
    echo
    echo "The old container is probably still serving -- 'docker compose ps' and"
    echo "'docker compose logs relayd' will say. Do NOT trust this deploy."
  } >&2
  exit 1
fi

echo
echo "Deployed ${GIT_SHA}: $health"
