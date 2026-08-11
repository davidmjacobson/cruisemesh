# Windows desktop app — design

Status: draft design, 2026-08-10. Two-round fleet design (codex Sol, Gemini
3.6 Flash, grok 4.5) adjudicated against source. Not yet sequenced into a
build; see "Sequencing" below.

Ships in two stages:

- **Stage 1 — headless mesh node** ("the elevator"): an always-on, plugged-in
  cabin PC that strengthens the mesh for the phones around it. No chat UI.
  Ships first; it is the value driver.
- **Stage 2 — full chat app**: the CruiseMesh messenger as a first-class
  Windows desktop app, reusing the Stage 1 node in one process.

---

## Decision: how to bind the core (and it differs from the plan's assumption)

**Recommendation: build Windows as a native Rust node that links
`cruisemesh-core` as an ordinary `lib` crate (no UniFFI on desktop). Stage 2
adds a Tauri UI hosted by that same Rust process. Reject Compose Desktop.**

This **contradicts the PRIVATE-TODO assumption** that a Windows GUI would
"reuse kotlin-gen bindings" via Compose Desktop. That assumption was tested
head-to-head by two model families and does not survive a source audit:

- The `kotlin-gen` UniFFI bindings **do** run on a desktop JVM today
  (`android/app/build.gradle.kts` points host unit tests at
  `cruisemesh_core.dll` via `uniffi.component…libraryOverride` + JNA). But that
  proves **binding mechanics for tests, not shell portability**.
- Source audit (verified against the tree): **170 Kotlin files / 40,223 lines**;
  the `mesh/` package is **16,044 lines**, ~75% in files with direct Android
  imports. Only ~5% of Compose files are source-portable without first building
  platform abstractions.
- The **Stage-1 value path is the *least* reusable part** — it's all Android
  platform APIs that must be rewritten regardless of binding:
  `LanTransport.kt` (1,892 L — `NsdManager`, `ConnectivityManager`),
  `MeshService.kt` (2,291 L — `Service`, `PowerManager`, BLE),
  `RelaySyncEngine.kt` (1,610 L — Android network/handlers).
- So Compose buys **almost no reuse for Stage 1**, while forcing a **JVM +
  JRE-bundle under an always-on native node** — the wrong runtime for the only
  piece that ships first, for the whole period it ships alone.
- Precedent already in-tree: **`relayd` links core as `path = "../core"`.**
  Desktop Stage 1 is the same pattern, different I/O. iOS already reimplements
  its shell in Swift (DESIGN.md §10 — thin native shells, not shared UI), so
  Compose Desktop would be a *third* shell dialect, not free reuse.

Binding-surface count stays at **two** (Kotlin + Swift); desktop is the same
language as core.

**Rejected alternatives:** Compose Desktop (above); Tauri-from-Stage-1 (Stage 1
has no UI — don't pull in WebView2 before it delivers value); a permanent
two-process Rust-node ↔ Compose-UI IPC split (invents a versioned node API +
second lifecycle around the store/identity/transports for UI reuse that isn't
real); native Rust GUI egui/slint (fine for the tray, too weak for a media-rich
messenger); C#/WinUI + `uniffi-bindgen-cs` (a third binding surface — stays
rejected).

---

## Stage 1 — the elevating node

A Windows PC is plugged in, awake, and stably networked, with none of a phone's
Doze/background limits. Stage 1 turns that into three concrete gains for the
family's phones:

1. **Stable LAN rendezvous** — always-on `_cruisemesh._tcp.` listener on
   **TCP 45892** (`LAN_DEFAULT_TCP_PORT`; verified), authenticated
   `Noise_XX_25519_ChaChaPoly_BLAKE2s` sessions via `LanNoiseSession`. Two
   phones that never overlap in time can both reach the PC.
2. **Never-sleeping DTN carry** — a message from phone A is held in the SQLite
   `MessageStore` until phone B proves receipt by authenticated digest (or
   normal expiry/eviction). This directly softens the mega-carrier problem.
3. **Relay bridge** — when the PC has ship internet, it uploads eligible
   family-carried envelopes and proxy-fetches mail for enrolled family phones,
   becoming the family's durable uplink.

### Transports (Stage 1 scope)

| Transport | Stage 1 | Why |
|---|---|---|
| Same-LAN mDNS + TCP | **Required** | Field-validated dominant ship path (DESIGN.md §5.4); Windows is ideal for a persistent listener |
| DTN carry / store-and-forward | **Required** | Turns sequential phone encounters into delivery |
| HTTPS relay sync | **Required** for full elevation | Durable uplink + mailbox proxy |
| **Windows BLE** | **De-scoped** | `btleplug` is central-only; WinRT GATT peripheral is hardware-dependent and poor for always-on. All three seats independently de-scoped it. LAN+relay+carry delivers the value. Revisit as an optional later spike. |

### Two hard problems the fleet surfaced (don't skip these)

**A. The `NetworkHelper` trust role — a public-release gate.** A beacon must
**not** appear on phones as an ordinary chat contact. If it did, a delivered
"✓✓" tick would imply the message reached a UI that doesn't exist. Add a shared
core trust classification (`Person | NetworkHelper`), a signed helper card
(`CMHELPER1`), and:
- exclude helpers from chat lists, friend directories, group-member pickers;
- retain a visible envelope addressed to a helper **without** claiming delivery
  and **never** relay-ack it until a real UI endpoint consumes it;
- a Stage-2 signed role-upgrade so a phone can promote "Cabin PC" to a normal
  contact deliberately.
For dev builds an ordinary contact is fine; the helper role is the public gate.

**B. Mutual-friending bootstrap.** A phone scanning the helper's QR does **not**
create mutual contacts — LAN Noise rejects an unknown `agree_pk` on both sides.
Needs an explicit state machine:
- **Path A (has internet at setup):** import Shore Pass (`CMRELAY1:`) → helper
  emits a friend card via `make_friend_card` carrying only a **deposit**-class
  token (never the member token) → phones scan → deposit `KIND_FRIEND_REQUEST`
  to the helper's mailbox → helper's relay poll fetches it → **headless delivery
  auto-imports** the contact → LAN Noise can now complete.
- **Path B (offline / no Shore Pass):** user imports each phone's friend card
  into the helper (paste / file); phones still scan the helper. Mutual contacts
  without relay; "reduced mode" (LAN mule only) until a Shore Pass is added.
- Auto-import **only** direct FRs (`shared: None`, card/sender match). Shared-tail
  and introduced FoF requests must **not** auto-create contacts.

### Headless inbound dispatch

Windows is greenfield — use the core `process_inbound_frame` authority directly
(not a port of Android's `InboundEnvelopeProcessor`). One dedicated store-executor
thread owns all `MessageStore` + `SeenIds` + delivery; LAN/relay threads only
enqueue. Seed `SeenIds` from history at start (Android `seedSeenIdsFromOwnHistory`
spirit). **Deliver → then commit**: never call `core_commit_inbound_delivery`
before durable kind handling succeeds. A small `mesh/delivery.rs` dispatches by
kind (friend request, receipt, LAN endpoint hint, relay update, and persist —
not notify — text/media for Stage 2 and ack correctness).

### Relay

Use **`CoreRelayPass` + a thin executor only** — no port of the legacy Kotlin
`RelaySyncEngine`, no third engine. The shell owns TLS sockets, timeouts,
bounded reads, cancellation, and the WebSocket lifecycle; the core owns request
paths, auth headers, relay selection, retry/rate-limit, cursors, ack eligibility,
and upload retirement (the seam Android's `CoreRelayDriver.kt` already demonstrates).

> **Migration dependency (independently found by codex, and it corroborates the
> plan's sequencing):** the current core relay pass ingests relay rows as
> *carried*, not *consumed*, pending the inbound-session migration
> (`store.rs`). Safe for a Stage-1 helper, but **Stage 2 user-delivery can't
> depend on it** until `mesh_receive`/`relay_pass` is the production path. This
> is the same reason the plan gates the GUI behind the core refactor.

### Invariants (mapped)

- **Endpoint privacy** — advertise only this device's own listener; seal endpoint
  hints pairwise; `lan_endpoint_host_is_local` on inbound; never forward a
  third-party address learned from mDNS/scan/cache/hint. The node is a
  store-and-forward rendezvous, **never** an endpoint directory or TCP broker.
- **DTN ack-safety** — ack only via `core_relay_ack_ids_with_consumed` (Consumed
  only; never on Expired alone); carried rows leave only via
  `core_confirm_carried_deliveries` on authenticated digest proof; a socket
  write / relay upload / "frame queued" is **never** receipt proof.
  `relay_uploaded_to` is an upload-suppression marker, not deletion evidence.
- **Credential split** — member token in DPAPI only; friend/helper cards carry a
  deposit-class token only.

### Lifecycle & packaging

- **Per-user tray process at login, not a Windows Service** (Session 0 can't do
  tray UI, toasts, camera, or per-user identity cleanly). Single-instance via
  named mutex + named pipe; crash-restart via Task Scheduler restart-on-failure.
- **Power is a first-class Stage-1 concern, not polish.** On AC, hold a documented
  system power request (`SetThreadExecutionState` / `PowerCreateRequest`); the
  display may sleep. **Never promise to override explicit lid-close / user sleep /
  Modern Standby** — persist before suspend, reconcile LAN+relay aggressively on
  resume, and say so honestly in the tray. A silently-sleeping elevator is the
  main failure mode.
- **Firewall** — ship Wi-Fi is usually the Public profile; the LAN listener needs
  an inbound rule (installer-declared or user-approved). Fail honest: "Incoming
  Wi-Fi blocked" in the tray, fall back to outbound/hint-driven.
- **Packaging** — unsigned zip for family dogfood first; signed NSIS/MSI installer
  when distributing wider. ⚠️ **Code-signing cert is a recurring yearly spend —
  David's call, flag before buying anything.** No WebView2 dependency in Stage 1.
- **CI** — adding a `desktop` crate must not break the ubuntu `cargo test
  --workspace`: set `default-members = ["core","relayd"]`, `--exclude` the desktop
  crate on ubuntu, `#[cfg(windows)]` + stub main so it links everywhere, and add a
  `windows-latest` job for `cargo test -p cruisemesh-node`. (`rust.yml` is
  ubuntu-only today.)

---

## Stage 2 — full messenger

Rehost the unchanged Stage 1 `NodeRuntime` inside a **Tauri** executable — the
Rust host stays the sole owner of identity, SQLite, transports, and scheduling;
TypeScript gets a narrow typed command/read-model/event API and **never** touches
the core crate, raw SQLite, keys, or transport credentials (strict Tauri
capabilities + CSP; treat all frontend input as untrusted). One authoritative
process, one `MessageStore`; the window hides to tray on close while the node
keeps running.

Scope: friending (QR display + webcam scan + `cruisemesh://` deep links + 4-word
fingerprint verify), 1:1 + groups, sealed text/media (`CoreAttachmentPayload`,
180 KiB blob limit — shell owns compression/capture/playback), delivery/read
ticks via `semantic.rs` (don't recompute in TS), native toast notifications
(notify only after durable commit; collapse by chat ID; reply / mark-read author
the same core envelopes as the composer — a release gate per ROADMAP), and
`.cmbak` backup/restore via `backup.rs`. Follow DESIGN.md §14 information
architecture; helper→Person promotion lives here. Use Android's screens, copy
(`strings.xml`), and behavior tests as the executable spec — port UX, not widgets.

Windows stays a **distinct identity** (multi-device identity is deferred,
DESIGN.md §13); restoring a phone's `.cmbak` onto the PC is a device *migration*
after retiring the phone, not a live clone.

---

## Milestone / PR sequence (Stage 1)

Order: friending → delivery → LAN → relay → polish. Each PR independently
reviewable.

1. Workspace scaffold + `windows-latest` CI (`--exclude` on ubuntu, cfg-gated).
2. DPAPI-backed `IdentityStore` (`encode/decode_identity_bytes`).
3. Relay config + Shore Pass import + deposit-safe friend cards (unit-test the
   deposit attenuation — member token never on a card).
4. Headless delivery dispatcher + `SeenIds` store executor (synthetic direct-FR
   auto-import; shared-tail must not create a contact).
5. LAN TCP + `LanNoiseSession` + accepted-contact gate (length-prefixed Noise
   records, max 65535; fail closed on unknown `agree_pk`).
6. HELLO/HELLO2 (`core_own_capabilities()`) + DIGEST + carry + confirm-before-
   delete; **W2 exit: pure mule traffic yields empty ack lists.**
7. `CoreRelayPass` thin executor; **Path A end-to-end** (Shore Pass + scan → FR
   over relay → contact; then LAN works).
8. mDNS + endpoint cache/hints + bounded `/24` sweep + firewall CTA.
9. Tray/console + autostart + power policy + crash-restart task.
10. Unsigned zip + dogfood README.
11. (later) Signed installer when distributing wider.
12. Frozen Stage-1 IPC (status / import card / import Shore Pass / events only).
13+. Stage 2 Tauri, after a short chat-IPC design addendum.

Stage-1 "done" = 1:1 LAN + relay elevation for phones that completed friending
with the helper. Group mule quality is best-effort foreign carry only
("invite Cabin PC to this group" is a phone-side + Stage-2 feature).

---

## Sequencing (unchanged from the plan)

Per PRIVATE-TODO (decided 2026-08-08), and independently corroborated by the
fleet's discovery of the `mesh_receive`/relay-pass migration gate: build the
headless node **after the core refactor freezes** so Stage 1 depends only on a
stable crate API (`MessageStore`, engine gates, `relay_wire`, `lan_session`) with
no UniFFI-regeneration gate on desktop. Stage 1 is the demand signal for Stage 2.

## Full source material

Exhaustive per-seat design docs and the two binding memos are on disk at
`C:\Users\david\fleet-reports\` (codex-winapp.md, grok-winapp.md, agy-winapp.json,
codex-binding.md, grok-binding.md). codex's Stage-1 doc is the deepest on the
`NetworkHelper` role and the migration gate; grok's is the most implementation-
ready (14-PR plan + acceptance tests). Gemini's misreported the LAN port as 43382
(actual 45892) — a reminder to trust these only where source agrees.
