# Build from the workspace root (docker compose sets context: ..).
FROM rust:1 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY relayd ./relayd
# The workspace manifest names every member, so cargo needs each member's
# Cargo.toml present even though this stage builds only relayd. The
# desktop crate joined the workspace after this file was written, which
# broke the on-box build with "failed to load manifest for workspace
# member /src/desktop" (found during the 2026-08-18 relay deploy).
COPY desktop ./desktop
# FR4: the build context above deliberately does not include `.git`, so
# `relayd/build.rs`'s `git rev-parse` fallback can't see the real commit
# from inside this stage. It has to be supplied per invocation:
#   docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD) relayd
# `tools/relay_deploy.sh` is the supported way to do that.
#
# There is deliberately no default. The old one was `unknown`, paired with
# `GIT_SHA: ${GIT_SHA:-unknown}` in compose and a static `GIT_SHA=...` line
# in the box's `relayd/.env` -- so a deploy that pulled new code and left
# `.env` alone built the new commit and reported the previous one on
# /healthz. A confident wrong answer is worse than no answer, and that one
# misdirected two real deploy investigations. An absent GIT_SHA now fails
# the build: here, cheaply, before cargo runs, and again inside build.rs
# through CRUISEMESH_REQUIRE_GIT_SHA, which additionally rejects a literal
# "unknown" and anything that is not shaped like a commit.
ARG GIT_SHA
ENV GIT_SHA=${GIT_SHA}
ENV CRUISEMESH_REQUIRE_GIT_SHA=1
RUN if [ -z "$GIT_SHA" ] || [ "$GIT_SHA" = "unknown" ]; then \
        echo "ERROR: the GIT_SHA build arg is missing or 'unknown'." >&2; \
        echo "This image would report the wrong commit on /healthz." >&2; \
        echo "Deploy with tools/relay_deploy.sh, or build with:" >&2; \
        echo '  docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD) relayd' >&2; \
        exit 1; \
    fi
RUN cargo build --release -p cruisemesh-relayd

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/cruisemesh-relayd /usr/local/bin/cruisemesh-relayd
# Absolute path by default so the process CWD cannot silently redirect the DB
# (see DEPLOY.md "CRUISEMESH_RELAY_DB path gotcha").
ENV CRUISEMESH_RELAY_BIND=0.0.0.0:8080
ENV CRUISEMESH_RELAY_DB=/data/cruisemesh-relayd.sqlite
EXPOSE 8080
VOLUME ["/data"]
CMD ["cruisemesh-relayd"]
