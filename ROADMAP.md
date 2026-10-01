# Roadmap

Milestones 0–6 are the history: each one de-risked the next, and
[DESIGN.md](DESIGN.md) §11 is their authoritative record. Everything under
"Where this goes next" is planned rather than done, and says how firmly.

**Where the project is right now:** both apps are public — [App
Store](https://apps.apple.com/app/id6789561040) and [Google
Play](https://play.google.com/store/apps/details?id=com.cruisemesh.app), free,
no account. One sailing has validated the ship-LAN transport; the instrumented
week has not happened yet.

## How it got here

| # | Milestone | What it proved | Status |
|---|---|---|---|
| 0 | Radio spike | iPhone↔Android background BLE is viable at all (the go/no-go gate) | ✅ Done |
| 1 | Core + 1:1 direct | Rust core, identity, QR friending, sealed text, ✓/✓✓/read over direct BLE | ✅ Done |
| 2 | Delay-tolerant delivery | Carry queue, sync digests, dedupe, cumulative receipts, mule delivery | ✅ Done |
| 3 | Internet relay | Self-hostable `relayd`, mixed BLE+relay delivery without duplicates | ✅ Done (see [relayd/DEPLOY.md](relayd/DEPLOY.md)) |
| 4 | Groups | Group keys and rotation, per-member ticks | 🔨 Groups shipped; per-member read aggregation open. Broadcast dropped from this milestone (DESIGN.md §6.6) |
| 5 | 🚢 Field test | Everything, on an actual cruise ship, for a week — latency, battery, and delivery-mode data | 🔨 One sailing validated the ship-LAN transport ([DESIGN.md](DESIGN.md) §5.4); the instrumented week is still ahead |
| 6 | Media attachments | Inline blobs (≤180 KiB) over any transport including relay | 🔨 Inline shipped; anything larger is the two-plane work below (DESIGN.md §8) |

Off the milestone track but shipped since: friends-of-friends introductions,
block and report, deliberate contact sharing, photo markup, push-to-talk voice
messages, and passphrase-encrypted local backup and restore.

## Where this goes next

Four bodies of work, in the order their dependencies allow. All four are
specified in writing; the ones whose specs live in this repository link to
them. None is committed to a date, and the last one may never be built.

### 1. Multi-device identity — specified, gated on three decisions

One person, several devices: a phone and a desktop, or an iPad and an iPhone.
Other people keep seeing one contact — device count and device identity are
never visible to them — and existing identities upgrade in place, so nobody
re-friends anyone. All four transports carry both contact traffic and
own-device sync, and two of a person's own devices never need to meet
directly.

This is the largest single change the project has planned, and it moves
multi-device out of the deferred list where it sat for the first year.

The plan front-loads a **forward-tolerance slice** that ships before anything
else: making today's fleet provably tolerant of fields, capability bits, and
card versions it does not yet understand, so the rest can land on its own
timeline without stranding installed builds. Everything after that is
sequenced core-first — identity split, ack and fan-out generalization, device
linking, own-device sync, revocation, then the shell surfaces — with the
Windows client joining as an ordinary linked device rather than anything
special.

Three decisions gate the middle of it: how strictly to scope sealing per
device, what the recovery-code experience looks like for a family, and the
per-person device cap. Nothing past the tolerance slice starts until those are
settled.

### 2. Media on two planes — specified, waiting on the session consolidation

Photos at full quality and video clips, kept deliberately *out* of the
delay-tolerant message pipeline. The message plane carries a small manifest
and thumbnail that behaves exactly like every other message — never lost,
carried, relayed, deduplicated. The bytes themselves ride a second plane that
recipients pull, with chunk bitmaps and resumable transfers, under explicit
consent and cost rules so nothing quietly spends someone else's battery,
storage, or cellular data.

Phase 1 is LAN-only and photos first, with no relay changes at all. Phase 2
adds relay blob endpoints so media works when the family is apart rather than
only when they are together. Phase 3 is polish, and a study — not a
commitment — of whether an opted-in, plugged-in device should courier
encrypted blobs for others.

Spec: [`specs/media-two-plane.md`](specs/media-two-plane.md).

### 3. Ship Wi-Fi compatibility reporting — specified, needs somewhere to go

Whether a given ship isolates guests from each other on its Wi-Fi decides
whether the fastest transport works at all, and only passengers can find out.
The design for collecting that is deliberately narrow: a user-initiated report
about one named ship, previewed in full before it is sent, containing a small
closed set of compatibility facts and never messages, contacts, network
addresses, Wi-Fi names, or a stable reporting identifier. No background
analytics, no periodic upload, no global telemetry switch.

The first two rollout phases need no server at all — golden fixtures and a
shared core reducer, then a guided local report exported through the platform
share sheet — so the work can start well before the question of where reports
aggregate is settled. Until it is, the field-report issue template is how this
data arrives.

Spec: [`specs/ship-wifi-field-reports.md`](specs/ship-wifi-field-reports.md).

### 4. Live push-to-talk — specified, and honestly may not be worth it

Real-time walkie-talkie voice over a friendly LAN, distinct from the
push-to-talk voice *messages* that already ship. It is buildable, and the
spec carries its own argument against itself: v1 would be 1:1, half-duplex,
both people in the foreground on the same non-isolated network — which is
exactly the network a cruise ship often does not provide. It costs a new
native codec dependency and a new capture pipeline to serve a case that async
voice messages already cover.

It stays on this list as a prototype-after-everything-else, with "never" a
legitimate outcome.

Spec: [`specs/live-ptt.md`](specs/live-ptt.md).

## Also ahead, independent of the four

- **Finish Milestone 4:** per-member read aggregation for group ticks.
- **Notification reliability as a release gate.** Background delivery must
  produce a timely local notification on real devices — screen off, battery
  saver, hours idle — before the app is offered beyond the development
  family. The incumbent apps' single most common failure is "the message
  arrived and nobody knew"; this project refuses to ship that.
- **Milestone 5 field instrumentation:** local-only logs measuring
  time-to-first-path, delivery latency, notification latency, and
  delivery-mode mix (direct / LAN / mule / relay). No telemetry — logs stay
  on the test devices.
- **Per-device relay credentials.** `relayd` currently authorizes fetch and
  acknowledgement at family granularity. Narrowing that to per-device
  capabilities lets a single device be cut off without rotating credentials
  for everyone, and is the relay-side half of multi-device revocation.
- **Windows client.** A tray node plus a messenger window, both Rust, sharing
  the core crate directly (`desktop/`). Dogfood today; it becomes a first-class
  linked device as part of the multi-device work.
- **A paid independent security review**, which the project has set for itself
  as a precondition before recommending CruiseMesh beyond its stated threat
  model ([SECURITY-DESIGN.md](SECURITY-DESIGN.md)).

## Deliberately deferred

Message-history sync for late group joiners, ratchet and post-quantum upgrades
(the envelope `version` byte reserves the path), relay federation, and a
broadcast channel scoped to one Shore Pass. See DESIGN.md §13.

## Non-goals

Anonymity and censorship resistance, stranger-to-stranger social features, and
real-time calls and presence. That includes the public broadcast channel: it
was designed, and it is not being built (DESIGN.md §6.6). Live push-to-talk is
the one real-time exception under consideration, and it is scoped as a
walkie-talkie between people who are already friends on a network that permits
it — not calling, not presence, and not certain to happen. See DESIGN.md §1.
