# CruiseMesh

Offline-first family messaging for cruise ships. See [DESIGN.md](DESIGN.md) for the
full design and milestone plan, [ROADMAP.md](ROADMAP.md) for the short version, and
[SECURITY-DESIGN.md](SECURITY-DESIGN.md) for what's encrypted and what isn't.

## Layout

- `core/` — Rust core (identity, crypto, sync engine) exposed to native shells via
  [UniFFI](https://mozilla.github.io/uniffi-rs/).
- `android/` — Android app (Kotlin, Jetpack Compose).
- `ios/` — iOS app (SwiftUI + CoreBluetooth), feature-parity shell; see [`ios/README.md`](ios/README.md).
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

**Core for iOS + Swift bindings** (macOS + Xcode required):

```sh
core/build-ios.sh
cd ios && xcodegen generate && open CruiseMesh.xcodeproj
```

See [`ios/README.md`](ios/README.md) for details.

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

## License

CruiseMesh is free software, licensed under the
[GNU AGPL-3.0-or-later](LICENSE). Everything here — the apps, the protocol,
and the relay server — is and stays open source and self-hostable.

Standing promises to users: no ads, no selling data or telemetry, no
paywalled encryption or receipts, no artificial limits on friends or groups.

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) (DCO sign-off
required; CLA for non-trivial changes).
