# Roadmap

The authoritative plan is [DESIGN.md](DESIGN.md) §11; this is the readable
summary. Milestones are sequential because each one de-risks the next.

**Where the project is right now:** both apps are at 1.0.7 and in release
testing (Play closed track, TestFlight), with store listings going public in
mid-August 2026.

| # | Milestone | What it proves | Status |
|---|---|---|---|
| 0 | Radio spike | iPhone↔Android background BLE is viable at all (the go/no-go gate) | ✅ Done |
| 1 | Core + 1:1 direct | Rust core, identity, QR friending, sealed text, ✓/✓✓/read over direct BLE | ✅ Done |
| 2 | Delay-tolerant delivery | Carry queue, sync digests, dedupe, cumulative receipts, mule delivery | ✅ Done |
| 3 | Internet relay | Self-hostable `relayd`, mixed BLE+relay delivery without duplicates | ✅ Done (see [relayd/DEPLOY.md](relayd/DEPLOY.md)) |
| 4 | Groups | Group keys and rotation, per-member ticks | 🔨 Groups shipped; per-member read aggregation open. Broadcast dropped from this milestone (DESIGN.md §6.6) |
| 5 | 🚢 Field test | Everything, on an actual cruise ship, for a week — latency, battery, and delivery-mode data | 🔨 One sailing validated the ship-LAN transport ([DESIGN.md](DESIGN.md) §5.4); the instrumented week is still ahead |
| 6 | Media attachments | Inline blobs (≤180 KiB) over any transport incl. relay — shipped; content-addressed chunk manifest for larger media — designed, not started | 🔨 Partially shipped (DESIGN.md §8) |

Off the milestone track but shipped since: friends-of-friends introductions,
block and report, deliberate contact sharing, and passphrase-encrypted local
backup and restore.

## Near-term focus

- Finish Milestone 4: per-member read aggregation for group ticks.
- Same-LAN transport: field-validated on a real sailing (Norwegian Jade — a
  non-isolated ship network, giving instant cross-ship delivery; see
  DESIGN.md §5.4). Next: gather client-isolation reports across more ships and
  cruise lines, so families know before sailing how well the app will work
  (the field-report issue template asks for this).
- **Notification reliability as a release gate:** background delivery must
  produce a timely local notification on real devices (screen off, battery
  saver, hours idle) before the app is offered to anyone beyond the
  development family. The incumbent apps' single most common failure is
  "the message arrived and nobody knew" — this project refuses to ship that.
- Milestone 5 field instrumentation: local-only logs measuring
  time-to-first-path, delivery latency, notification latency, and
  delivery-mode mix (direct / LAN / mule / relay). No telemetry — logs stay on
  the test devices.
- A paid independent security review, which the project has set for itself as
  a precondition before recommending CruiseMesh beyond its stated threat model
  ([SECURITY-DESIGN.md](SECURITY-DESIGN.md)).

## Deliberately deferred

Multi-device identity, message-history sync for late group joiners,
ratchet/post-quantum upgrades (the envelope `version` byte reserves the
path), relay federation, and a broadcast channel scoped to one Cruise Pass.
See DESIGN.md §13.

## Non-goals

Anonymity/censorship resistance, real-time features (typing indicators,
calls, presence), and stranger-to-stranger social features. That last one now
includes the public broadcast channel: it was designed, and it is not being
built (DESIGN.md §6.6). See DESIGN.md §1.
