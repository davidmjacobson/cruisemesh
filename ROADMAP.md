# Roadmap

The authoritative plan is [DESIGN.md](DESIGN.md) §11; this is the readable
summary. Milestones are sequential because each one de-risks the next.

| # | Milestone | What it proves | Status |
|---|---|---|---|
| 0 | Radio spike | iPhone↔Android background BLE is viable at all (the go/no-go gate) | ✅ Done |
| 1 | Core + 1:1 direct | Rust core, identity, QR friending, sealed text, ✓/✓✓/read over direct BLE | ✅ Done |
| 2 | Delay-tolerant delivery | Carry queue, sync digests, dedupe, cumulative receipts, mule delivery | ✅ Done |
| 3 | Internet relay | Self-hostable `relayd`, mixed BLE+relay delivery without duplicates | ✅ Done (see [relayd/DEPLOY.md](relayd/DEPLOY.md)) |
| 4 | Groups + broadcast | Group keys and rotation, per-member ticks, public broadcast channel | 🔨 In progress |
| 5 | 🚢 Field test | Everything, on an actual cruise ship, for a week — latency, battery, and delivery-mode data | ⏳ Upcoming |
| 6 | Media attachments | Inline blobs (≤180 KiB) over any transport incl. relay — shipped; content-addressed chunk manifest for larger media — designed, not started | 🔨 Partially shipped (DESIGN.md §8) |

## Near-term focus

- Finish Milestone 4 (groups, broadcast).
- Field-test the authenticated same-LAN TCP transport across Android and iOS,
  including permissive and client-isolated captive Wi-Fi.
- **Notification reliability as a release gate:** background delivery must
  produce a timely local notification on real devices (screen off, battery
  saver, hours idle) before the app is offered to anyone beyond the
  development family. The incumbent apps' single most common failure is
  "the message arrived and nobody knew" — this project refuses to ship that.
- Milestone 5 field instrumentation: local-only logs measuring
  time-to-first-path, delivery latency, notification latency, and
  delivery-mode mix (direct / mule / relay). No telemetry — logs stay on
  the test devices.

## Deliberately deferred

Multi-device identity, message-history sync for late group joiners,
ratchet/post-quantum upgrades (the envelope `version` byte reserves the
path), passworded broadcast channels, relay federation. See DESIGN.md §13.

## Non-goals

Anonymity/censorship resistance, real-time features (typing indicators,
calls, presence), and stranger-to-stranger social features beyond the
clearly-labeled broadcast channel. See DESIGN.md §1.
