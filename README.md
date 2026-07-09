# CruiseMesh

Offline-first family messaging for cruise ships. See [DESIGN.md](DESIGN.md) for the
full design and milestone plan.

## Layout

- `core/` — Rust core (identity, crypto, sync engine) exposed to native shells via
  [UniFFI](https://mozilla.github.io/uniffi-rs/).
- `android/` — Android app (Kotlin, Jetpack Compose), currently Milestone 0/1 scaffold.
- `relayd/` — Axum + SQLite relay mailbox server for Milestone 3.

## Building

**Workspace tests:**

```sh
cargo test --workspace
```

**Core for Android + Kotlin bindings** (requires `rustup target add
aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`, `cargo install
cargo-ndk`, and the Android NDK):

```sh
core/build-android.sh
```

Run this after changing anything in `core/`; it regenerates
`android/app/src/main/kotlin-gen` and `android/app/src/main/jniLibs`.

**Android app:**

```sh
cd android
./gradlew assembleDebug
```

**Relay server** (local dev):

```sh
# PowerShell — use an *absolute* DB path (relative defaults resolve against CWD
# and are easy to mis-watch; see relayd/DEPLOY.md).
$env:CRUISEMESH_RELAY_TOKENS="family-token"
$env:CRUISEMESH_RELAY_DB="$PWD\tmp\relayd-dev.sqlite"
cargo run -p cruisemesh-relayd
```

The relay API stores the full public envelope header shape
(`msg_id`, `hop_ttl`, `recipient_hint`, `sealed`, `expiry_ms`) with
msg_id-based dedupe per family token, fetch-by-hint + cursor, delete-on-ack,
per-envelope expiry pruning, and a 30-day server retention ceiling. It is
content-agnostic (sealed blobs only — text and receipt envelopes share one path).
`GET /ws` adds WebSocket push: same family bearer (header or `?token=`), same
`hints=`/`after=` as poll, replay-then-stream of matching envelopes as JSON
pages identical to the REST fetch body; acks stay on `POST /envelopes/ack`, and
slow subscribers are dropped so reconnect+replay can heal (bounded memory).

Optional env vars:

- `CRUISEMESH_RELAY_BIND` — listen address, default `0.0.0.0:8080`
- `CRUISEMESH_RELAY_DB` — SQLite path, default `cruisemesh-relayd.sqlite` (**prefer absolute**)
- `CRUISEMESH_RELAY_TOKENS` — required comma-separated family bearer tokens

**Production deploy** (Docker + Caddy TLS, family-token provisioning, DB-path
gotcha): see [`relayd/DEPLOY.md`](relayd/DEPLOY.md) and
[`relayd/docker-compose.yml`](relayd/docker-compose.yml).
