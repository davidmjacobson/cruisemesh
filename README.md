<p align="center">
  <img src="https://cruisemesh.app/og.png" alt="CruiseMesh — text your family when there's no signal" width="640">
</p>

# CruiseMesh

**Text your family when there's no signal.**

CruiseMesh carries end-to-end encrypted messages between phones over the ship's
Wi-Fi and Bluetooth, so nobody has to buy an internet package. It's built for
cruise ships and anywhere coverage is unreliable. There are no accounts and no
phone numbers — identity is a keypair generated on your phone — so it runs on a
hand-me-down phone with no SIM or a Wi-Fi-only tablet.

[![Rust](https://github.com/davidmjacobson/cruisemesh/actions/workflows/rust.yml/badge.svg)](https://github.com/davidmjacobson/cruisemesh/actions/workflows/rust.yml)
[![iOS](https://github.com/davidmjacobson/cruisemesh/actions/workflows/ios.yml/badge.svg)](https://github.com/davidmjacobson/cruisemesh/actions/workflows/ios.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)

A Rust core with native iOS and Android shells, plus a relay server you can run
yourself. It's all AGPL.

## Why this exists

On a cruise with family, sending a message meant buying the cruise line's
$7-per-phone, per-day messaging plan — which isn't even internet access. Four
people on a seven-day cruise is $196 to coordinate dinner and say "I'm back on
board."

Phones can already talk to each other without the internet: over the ship's
Wi-Fi when it allows it, over Bluetooth when they're nearby, and by carrying
sealed messages along until paths cross. CruiseMesh is what happens if family
messaging just uses the connections already in your pocket.

There's a 75-second [explainer video](https://cruisemesh.app/#how) if you'd
rather watch than read.

## How messages get through

Messages are sealed on the sending phone and delivered by whichever route
exists. Minutes of delay are expected and fine; the whole protocol assumes
hours-scale worst case, out-of-order arrival, and duplicates.

| Route | What it is |
|---|---|
| **Ship Wi-Fi (same LAN)** | Phones associated to the ship's network find each other over Bonjour/NSD and talk directly over TCP, no internet package. Instant and ship-wide where the network doesn't isolate clients — the dominant transport on the one sailing this has been field-tested on. |
| **Direct Bluetooth** | Phones in BLE range exchange messages immediately. Both GATT roles run at once, so an Android central can wake a backgrounded iPhone peripheral. |
| **Carried along** | A family member's phone picks up your queued message and physically carries it until it meets the recipient. Classic delay-tolerant networking, and on a ship where everyone orbits the same buffet it works better than it sounds. |
| **Internet relay** | When any one phone gets online, it syncs the whole family's queue — including mail it's carrying for others. One paid Wi-Fi package becomes the family's uplink, and reaches family at home who aren't on the ship at all. |

None of these change the crypto, the storage, the receipts, or the
deduplication. The sync engine hands sealed envelopes to whatever links happen
to be up. [DESIGN.md](DESIGN.md) §3–§5 covers the physics and the transports;
§9 covers the relay.

## Try it without a ship

Install CruiseMesh on two phones and add each other as friends. Put both in
airplane mode, switch Bluetooth back on (airplane mode turns it off), and send
a message. It arrives with no internet involved at all. That is the same
delivery path a family uses at sea.

## Status

Both apps are at **1.0.7** and in release testing — Play closed track and
TestFlight — with store listings going public in mid-August 2026. The Rust
workspace, the Android unit suite, and the iOS build and test suite gate every
pull request.

What works today: 1:1 and group messaging, delivery and read receipts, photos
and voice memos (inline, up to 180 KiB), QR friending with a spoken 4-word
fingerprint, friends-of-friends introductions, all four delivery routes above,
block and report, and passphrase-encrypted local backup and restore.

What isn't here: chunked transfer for media too large to inline, multi-device
identity, and history sync for someone who joins a group late. There is no
public broadcast channel and there won't be one — a channel strangers can post
to is a moderation surface this project isn't going to grow (DESIGN.md §6.6).
[ROADMAP.md](ROADMAP.md) has the milestone view; DESIGN.md §13 explains the
deferrals.

**No independent security review has happened yet.** The design is deliberately
boring — libsodium primitives used whole, no bespoke constructions, the whole
thing fits on one page — but nobody unaffiliated has tried to break it. Read
[SECURITY-DESIGN.md](SECURITY-DESIGN.md) before trusting it beyond family
logistics.

## Security and privacy

- Messages are signed then sealed per-message and padded into 256-byte buckets.
  Relays and other phones only ever see ciphertext.
- Receipts are ordinary sealed envelopes, so nothing on the wire reveals read
  state.
- The envelope's `recipient_hint` rotates daily, so there's no stable
  identifier on the wire for a relay or an observer to follow.
- The apps contain no analytics, advertising, or crash-reporting SDKs.
- The adversary this is built for is "no internet," not a nation-state. It does
  not attempt anonymity, censorship resistance, or resistance to a global
  passive observer.

[SECURITY-DESIGN.md](SECURITY-DESIGN.md) is the standalone version of all of
this, including what leaks and what was traded away. To report a hole, see
[SECURITY.md](SECURITY.md) — privately, please.

## Internet delivery, and how the project is paid for

The relay (`relayd/`) is a deliberately dumb mailbox: sealed envelopes and
routing hints, delete-on-ack, 30-day ceiling. It's a single Rust binary plus
SQLite that runs on a $4/month VPS, and **running your own is free and always
will be** — [`relayd/DEPLOY.md`](relayd/DEPLOY.md) is the whole recipe.

For families who don't want to run a server, the same binary is offered as a
hosted pass at [cruisemesh.app](https://cruisemesh.app) for $9.99. That is the
project's only revenue. It buys nothing the code doesn't already give you for
free — the mesh needs no purchase, encryption and receipts are never paywalled,
and there's no limit on friends or groups.

Standing promises: no ads, no selling data, no telemetry, no paywalled
encryption or receipts, no artificial limits.

## Repository layout

- `core/` — Rust core: identity, crypto, message store, sync engine, framing,
  transport policy. Exposed to both shells via
  [UniFFI](https://mozilla.github.io/uniffi-rs/). Shared behavior lives here,
  never in one platform's shell.
- `android/` — Android app (Kotlin, Jetpack Compose).
- `ios/` — iOS app (SwiftUI + CoreBluetooth); see [`ios/README.md`](ios/README.md).
- `relayd/` — the relay mailbox server (Axum + SQLite, Docker).
- `specs/` — protocol specs for individual features (same-LAN transport,
  friends-of-friends, group management, friend-card format).
- `fuzz/` — `cargo-fuzz` targets for the pre-authentication decoders; see
  [`fuzz/README.md`](fuzz/README.md).

## Building

Rust workspace — core and relay:

```sh
cargo test --workspace
```

Run `cargo fmt --all` before committing anything Rust; CI treats formatting as
a hard gate.

**Android.** Regenerating the Kotlin bindings and native libraries needs
`rustup target add aarch64-linux-android armv7-linux-androideabi
x86_64-linux-android`, `cargo install cargo-ndk`, and the Android NDK:

```sh
core/build-android.sh          # after any change under core/
cd android && ./gradlew assembleDebug
```

That script regenerates `android/app/src/main/kotlin-gen` and
`android/app/src/main/jniLibs` together and stamps both, so Gradle can detect a
stale or partial regeneration and refuse to build. For JVM unit tests alone
there's a faster host-only path — see [AGENTS.md](AGENTS.md).

**iOS** (macOS + Xcode):

```sh
core/build-ios.sh
cd ios && xcodegen generate && open CruiseMesh.xcodeproj
```

The Swift bindings under `ios/CruiseMesh/Generated/` are checked in and can be
regenerated from any platform; only compiling the Swift needs a Mac. Details in
[`ios/README.md`](ios/README.md) and [AGENTS.md](AGENTS.md).

**Relay server**, local development:

```sh
# Use an absolute DB path — relative defaults resolve against the working
# directory and are easy to mis-watch. See relayd/DEPLOY.md.
export CRUISEMESH_RELAY_TOKENS="family-token"
export CRUISEMESH_RELAY_DB="$PWD/tmp/relayd-dev.sqlite"
cargo run -p cruisemesh-relayd
```

`CRUISEMESH_RELAY_BIND` sets the listen address (default `0.0.0.0:8080`).
`CRUISEMESH_RELAY_TOKENS` is required. Production deployment — Docker, Caddy
TLS, token provisioning, quotas — is [`relayd/DEPLOY.md`](relayd/DEPLOY.md);
the API and its ack rules are DESIGN.md §9.

## Documentation

| Document | What's in it |
|---|---|
| [DESIGN.md](DESIGN.md) | The architecture and the reasoning behind it. Start here. |
| [ROADMAP.md](ROADMAP.md) | Milestones and near-term focus, in one page. |
| [SECURITY-DESIGN.md](SECURITY-DESIGN.md) | What's encrypted, what leaks, what was traded away. |
| [SECURITY.md](SECURITY.md) | How to report a vulnerability. |
| [CONTRIBUTING.md](CONTRIBUTING.md) | DCO, CLA, and what makes a good PR here. |
| [AGENTS.md](AGENTS.md) | Build and bindgen recipes, including the fast paths. |
| [`relayd/DEPLOY.md`](relayd/DEPLOY.md) | Running a relay in production. |
| `specs/` | Per-feature protocol specs. |

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Every
commit needs a DCO sign-off (`git commit -s`), and non-trivial changes need
agreement to [CLA.md](CLA.md). The [Code of Conduct](CODE_OF_CONDUCT.md)
applies everywhere in the project.

Two rules worth knowing before you write code: no new cryptographic
constructions, ever (libsodium primitives whole — Bridgefy is the cautionary
tale), and anything touching envelopes, receipts, dedupe, or sync digests needs
a headless test in `core/`. That's how the sync engine stays trustworthy
without two phones in hand.

Field reports are worth more to this project than most code. If you run this on
a ship, a hike, or a festival, there's an issue template that asks for
delivery-mode mix, latency, battery, and device models. Whether a given ship's
Wi-Fi isolates clients from each other is something only passengers can find
out.

## License

[GNU AGPL-3.0-or-later](LICENSE). The apps, the protocol, and the relay server
are open source and self-hostable, and will stay that way.
