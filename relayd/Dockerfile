# Build from the workspace root (docker compose sets context: ..).
FROM rust:1 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY relayd ./relayd
# FR4: the build context above deliberately does not include `.git`, so
# `relayd/build.rs`'s `git rev-parse` fallback can't see the real commit
# from inside this stage. Pass it in explicitly:
#   docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD)
# Falls back to "unknown" (build.rs's own default) if not provided.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}
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
