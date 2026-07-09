# Build from the workspace root (docker compose sets context: ..).
FROM rust:1 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY relayd ./relayd
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
