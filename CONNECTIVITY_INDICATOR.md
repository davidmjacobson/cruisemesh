# Connectivity Indicator — per-contact reachability at a glance

**Status: design, not yet implemented.** This doc is written to be handed to an
implementation agent (Sonnet / codex). It specifies WHAT to build and WHERE;
follow existing code patterns in the named files for HOW.

**Dependency:** the `MESH_CARRY` tier (§2) assumes 1:1 BLE muling works — see
`BLE_1TO1_MULING.md`. Land that first (or ship Phase 1 with `MESH_CARRY`
disabled until it lands).

## 1. Goal

A user glancing at the app should roughly know, without sending anything:

- **Who is reachable right now**, and via what path (direct Bluetooth vs. the
  internet relay), i.e. "how likely is my message to be delivered promptly?"
- **Whether their own device is well-connected** (mesh up, peers nearby, relay
  reachable) — an upgrade of the current "Mesh running" pill, which today only
  reports service state, not actual connectivity.

Scale target: one relay family/friend group, O(100) contacts. The design adds
**zero new radio wakeups and zero new timers** — every new signal piggybacks on
traffic the app already generates (BLE HELLOs, the existing 60 s relay poll).

Android first. The relay protocol addition (§6) is client-agnostic so iOS can
adopt it later unchanged.

---

## 2. Reachability model

### 2.1 Per-contact levels

One enum, computed in a new **pure, unit-testable** class (no Android deps,
same pattern as `MeshRouterState` / `ReconnectBackoffTracker`):

```kotlin
// android/app/src/main/kotlin/com/cruisemesh/app/mesh/ContactReachability.kt
enum class ReachabilityLevel {
    NEARBY,        // direct BLE link to this contact, HELLO'd, right now
    ONLINE_RELAY,  // their device synced with the relay very recently AND our relay path works
    RECENT,        // heard from them (BLE or relay presence) within RECENT_WINDOW_MS
    MESH_CARRY,    // not reachable directly, but ≥1 HELLO'd BLE peer will mule the message
    OFFLINE,       // none of the above — message will queue for later delivery
}
```

Precedence is top-down: `NEARBY` beats `ONLINE_RELAY` beats `RECENT` beats
`MESH_CARRY` beats `OFFLINE`.

`MESH_CARRY` exists because of `BLE_1TO1_MULING.md`: once that ships, any
connected BLE peer — contact or stranger — carries a 1:1 message onward
(hands it to the recipient when they meet, or uplinks it to the relay the
moment they have internet). It is deliberately the second-weakest tier: the
message *leaves your phone* promptly, but end-to-end delivery still depends
on the mule's future connectivity.

### 2.2 Exact computation rules

Inputs (all defined in §4/§5):

| Input | Source |
|---|---|
| `directLink: Boolean` | `MeshRouter.routeFor(userId) != null` (already exists) |
| `peerLastSeenMs: Long?` | max of: relay presence `last_seen` for this contact (§6), last HELLO from this contact on any link, timestamp of last message/receipt received from this contact |
| `selfRelayHealthy: Boolean` | our last relay sync pass succeeded within `2 * RELAY_POLL_INTERVAL_MS` and `hasValidatedInternet()` is true |
| `nearbyPeerCount: Int` | `MeshConnectivityStatus.nearbyPeerIds.size` — distinct HELLO'd peers, contacts or strangers |
| `nowMs: Long` | injected clock (for tests) |

```
NEARBY        ⇐ directLink
ONLINE_RELAY  ⇐ selfRelayHealthy && presenceLastSeen != null
                 && now - presenceLastSeen <= PRESENCE_ONLINE_WINDOW_MS
RECENT        ⇐ peerLastSeenMs != null && now - peerLastSeenMs <= RECENT_WINDOW_MS
MESH_CARRY    ⇐ nearbyPeerCount > 0
OFFLINE       ⇐ otherwise
```

(`MESH_CARRY` uses *any* HELLO'd peer, not just contacts — post-muling, a
stranger CruiseMesh phone is a valid best-effort carrier too, same as for
group traffic.)

Constants (put in `ContactReachability`, referenced by tests):

```kotlin
// 2.5 × the 60 s relay poll: "their phone is actively syncing right now."
const val PRESENCE_ONLINE_WINDOW_MS = 150_000L
// "Was around a moment ago; delivery likely soon, not instant."
const val RECENT_WINDOW_MS = 15 * 60_000L
```

`ONLINE_RELAY` deliberately requires **our own** relay health too: if we can't
reach the relay, their presence freshness is both stale data and a path we
can't use, so advertising it would over-promise.

### 2.3 Honesty constraints (do not violate)

- **`MESH_CARRY` is gated on 1:1 BLE muling actually shipping**
  (`BLE_1TO1_MULING.md`). Until `RealMeshSender` sprays undeliverable 1:1
  envelopes to connected peers, "some peer nearby" does NOT move a 1:1
  message, and showing this tier would be a lie. Implement the tier behind a
  single boolean constant (`MESH_CARRY_ENABLED` in `ContactReachability`)
  flipped on in the muling PR or after it lands.
- `MESH_CARRY` copy must promise carriage, not delivery: the message leaves
  this phone, but arrival depends on the mule later reaching the recipient or
  the relay.
- The indicator is probabilistic, and copy must say so: "likely", "will deliver
  when reachable" — never "delivered instantly".
- Never show a level the router can't act on. `NEARBY` must come from the same
  `routeFor` the send path uses, so green == "a send right now would take the
  BLE path".

### 2.4 Groups

- Group row in the chat list: badge shows the **best** level among members
  (excluding self).
- Group chat header: text `"{n} of {m} reachable"` where n = members at
  `NEARBY` or `ONLINE_RELAY`, m = member count excluding self.
- For groups (unlike 1:1), a `NEARBY` non-member peer genuinely can carry the
  flood — but keep v1 simple: aggregate member levels only.

---

## 3. UX spec

### 3.1 Presence dot on avatars (chat list + chat screens)

Add an optional badge to `AvatarBadge`
(`android/app/src/main/kotlin/com/cruisemesh/app/ui/AvatarBadge.kt`):

- New parameter `reachability: ReachabilityLevel? = null` (null = don't render
  a badge at all — used for own avatar, previews, onboarding).
- Rendered as a circle at the avatar's bottom-right, diameter = `size * 0.28`
  (≈13 dp on the 48 dp list avatar), with a 2 dp border of
  `MaterialTheme.colorScheme.surface` so it reads on top of photos.
- Colors (add semantic colors in `CruiseMeshTheme.kt`, don't hardcode):

| Level | Fill | Notes |
|---|---|---|
| `NEARBY` | green (`#2E7D32` light / `#81C784` dark) | filled |
| `ONLINE_RELAY` | blue (`#1565C0` light / `#64B5F6` dark) | filled |
| `RECENT` | amber (`#F9A825` light / `#FFD54F` dark) | filled |
| `MESH_CARRY` | amber, **hollow ring** (2 dp stroke, surface-color center) | shape ≠ fill distinguishes it from `RECENT` without relying on a 4th color |
| `OFFLINE` | no dot | absence = offline; keeps the list calm |

- Accessibility: extend the avatar's `contentDescription`:
  `"Avatar for Maya. Nearby via Bluetooth"` / `"… Online via relay"` /
  `"… Recently active"` / `"… Reachable through the mesh"` / (no suffix when
  offline). Color is never the only
  channel: the chat header (§3.3) and contact sheet (§3.4) carry the words.

### 3.2 Mesh status pill upgrade

`MeshStatusPill` (`ui/MeshStatusPill.kt`) gains a leading status dot and richer
text. New signature:

```kotlin
fun MeshStatusPill(text: String, dotColor: Color?, onClick: () -> Unit, modifier: Modifier)
```

`MainActivity` composes the text from three axes (state × nearby count × relay
health) — pill stays one line, middle-dot separated:

| Situation | Pill text | Dot |
|---|---|---|
| Mesh active, 3 distinct HELLO'd peers, relay OK | `Mesh on · 3 nearby · relay ✓` | green |
| Mesh active, 0 peers, relay OK | `Mesh on · relay ✓` | blue |
| Mesh active, 2 peers, no internet | `Mesh on · 2 nearby · no internet` | green |
| Mesh active, 0 peers, no internet | `Mesh on · offline` | amber |
| Mesh active, relay configured but failing | `Mesh on · relay unreachable` | amber |
| Mesh active, no relay configured | `Mesh on · no relay set up` | amber |
| `NO_BLUETOOTH` | existing label unchanged | gray |
| `STARTING` / `STOPPED` | existing labels unchanged | gray |

"Nearby" count = **distinct HELLO'd userIds**, not link count — every phone
runs dual BLE roles, so the same peer often holds two links (see
`MeshRouterState` KDoc). Requires the new `helloedUserIds()` in §5.1.

Existing `transientMeshStatus` override in `MainActivity.kt:482` keeps
priority over all of this.

### 3.3 Chat screen header (1:1)

Under the contact name in the `ChatScreen` top bar, a one-line
`bodySmall`/`onSurfaceVariant` status:

| Level | Copy |
|---|---|
| `NEARBY` | `Nearby via Bluetooth` |
| `ONLINE_RELAY` | `Online via relay` |
| `RECENT` | `Active {n}m ago` (round to minutes; ≥60 m → `Active {h}h ago`) |
| `MESH_CARRY` | `Nearby phones will carry your message` |
| `OFFLINE` | `Offline — will deliver when reachable` |

Group chat header gets the `{n} of {m} reachable` line (§2.4).

### 3.4 Contact details sheet

`ContactDetailsSheet.kt`: add a "Connectivity" row showing the same copy as
§3.3 plus the path detail when known, e.g. `Last seen via relay 12 min ago`.

### 3.5 Refresh behavior

Levels decay with time (a contact drifts `ONLINE_RELAY` → `RECENT` → `OFFLINE`
with no event firing), so the UI must re-evaluate on a clock: a 30 s
`LaunchedEffect` ticker in `MainActivity` (active only while the activity is
RESUMED) that bumps a `nowMs` state consumed by the level computation. No
background work.

---

## 4. New observable state: `MeshConnectivityStatus`

New file `mesh/MeshConnectivityStatus.kt`, same process-wide object pattern as
`MeshRuntimeStatus`:

```kotlin
object MeshConnectivityStatus {
    /** Distinct HELLO'd peer userIds (hex via UserIdHex.encode), any contact or stranger. */
    val nearbyPeerIds: StateFlow<Set<String>>

    /** Relay health, updated by MeshService's sync pass + network callbacks. */
    val relay: StateFlow<RelayHealth>   // sealed: Ok(lastSyncMs) | NoInternet | NoConfig | Failing(lastAttemptMs)

    /** hex userId -> epoch ms we last had evidence the contact's device was alive. */
    val contactLastSeen: StateFlow<Map<String, Long>>

    // Mutators mirror MeshRuntimeStatus style: setNearbyPeers(...), setRelayHealth(...),
    // mergeLastSeen(userIdHex, seenAtMs) (keep max), clear() on service stop.
}
```

Publish points — all inside `MeshService`, which already owns every relevant
event:

| Event | Where in MeshService | Publish |
|---|---|---|
| HELLO received | `handleHello` (~line 959), after `MeshRouter.onHello` | `setNearbyPeers(MeshRouter.helloedUserIds())`; `mergeLastSeen(peer, now)` |
| Central/peripheral disconnect | the two callbacks at ~lines 900/909, after `MeshRouter.onDisconnected` | `setNearbyPeers(...)` |
| Mesh roles stop / service destroy | `stopMeshRoles` / `onDestroy` | `setNearbyPeers(emptySet())`, `clear()` on destroy |
| Relay sync pass completes | end of `performRelaySyncPass` (~line 562) | `setRelayHealth(Ok(now))`, plus per-contact presence merges (§6.4) |
| Relay sync pass throws | catch in `requestRelaySync` (~line 522) | `setRelayHealth(Failing(now))` |
| No validated internet | `requestRelaySync` early-return; also `onLost` in `relayNetworkCallback` | `setRelayHealth(NoInternet)` |
| No relay config | `performRelaySyncPass` `configs.isEmpty()` return | `setRelayHealth(NoConfig)` |
| Message/receipt received from a contact (BLE or relay) | the inbound handling paths that already resolve a sender userId | `mergeLastSeen(sender, now)` |

`MainActivity` collects these flows exactly the way it already collects
`MeshRuntimeStatus.state`, computes `ReachabilityLevel` per `ChatSummary` via
`ContactReachability`, and threads levels into `ChatListScreen` /
`ChatScreen` / `ContactDetailsSheet`.

---

## 5. Small extensions to existing classes

### 5.1 `MeshRouterState` / `MeshRouter`

Add (with the same synchronized style):

```kotlin
/** Distinct HELLO'd peer userIds, hex-encoded — dual-role duplicate links collapse to one. */
fun helloedUserIds(): Set<String>
```

`MeshRouter` forwards it. Unit-test the dual-role dedupe in
`MeshRouterStateTest` (two addresses, same userId → set of 1; disconnect one →
still 1; disconnect both → 0).

### 5.2 `ChatSummary` / `ChatListScreen`

- `ChatSummary` gains `val reachability: ReachabilityLevel = OFFLINE` (include
  it in the hand-written `equals` so rows recompose on change).
- `ChatRow` passes it to `AvatarBadge`.
- `ChatListScreen` previews updated to show each level (previews are the de
  facto visual spec).

---

## 6. Relay presence protocol (the "ping" — Phase 2)

The user-visible question "is their phone currently syncing with the relay?"
needs one new relay primitive. Do **not** implement presence as sealed
envelopes between contacts: N contacts beaconing each other every few minutes
is O(N²) mailbox writes, junk that every device must fetch and ack. Instead:

**Piggyback on the existing 60 s poll.** Every device already talks to the
relay once a minute when it has internet (`RELAY_POLL_INTERVAL_MS = 60_000`,
`MeshService.kt:108`). One extra tiny HTTP call per pass gives everyone ≤60 s
presence freshness with zero new wakeups.

### 6.1 Endpoint (relayd)

Add to `relayd/src/lib.rs` router (`app()`, line ~383):

```
POST /presence            (same Bearer family-token auth as /envelopes)
{
  "announce": ["<b64url hint>", ...],   // my own recent-day hints (≤4)
  "query":    ["<b64url hint>", ...]    // my contacts' recent-day hints (≤512)
}
→ 200
{
  "now_ms": <server clock>,
  "presence": [ {"hint": "<b64url>", "last_seen_ms": <server clock ms>}, ... ]
}
```

- On `announce`: upsert `last_seen_ms = now` for each hint, scoped to the
  family token.
- On `query`: return rows the server has; omit unknown hints.
- **Server timestamps only.** Client computes `age = now_ms - last_seen_ms`
  from the same response, so device clock skew cancels out entirely. Store
  locally as `localNow - age`.
- Schema: `presence(family_token TEXT, hint BLOB, last_seen_ms INTEGER, PRIMARY KEY(family_token, hint))`.
- Prune rows older than 48 h inside the existing `prune_expired` pass (day-salt
  rotation makes older hints meaningless anyway).
- Reuses the §6.4 rotating `recipient_hint`
  (`BLAKE2b-8(UserID ‖ day-number)`) — no new identifier class on the wire.

### 6.2 Privacy note (document in DESIGN.md §6.4 when implementing)

Today the relay sees a mixed bag of self + proxy hints per fetch and can't
cleanly tell which hint is the caller. An explicit `announce` removes that
ambiguity: the relay now knows "this connection = this (rotating, per-day)
hint" and its online pattern. Within the family-scale threat model (§9: relay
already learns traffic timing and social-graph size) this is an acceptable,
deliberate trade for the feature. Two mitigations ship with it:

- Hints still rotate daily; the relay can't link across days without extra work.
- A user toggle (§6.5) turns `announce` off entirely.

### 6.3 Client: `RelayClient`

```kotlin
data class RelayPresence(val hint: ByteArray, val lastSeenMs: Long)
data class RelayPresencePage(val nowMs: Long, val presence: List<RelayPresence>)

fun syncPresence(config: RelayConfig, announce: List<ByteArray>,
                 query: List<ByteArray>, network: Network? = null): RelayPresencePage
```

Same Gson/`openConnection` plumbing as the existing calls (network pinning
included — presence must ride `relayBindTarget()` like everything else).

### 6.4 Client: `MeshService.performRelaySyncPass`

After the mailbox poll, per distinct relay config:

- `announce = recentHintsFor(identity.userId, now)` — skipped when the §6.5
  toggle is off.
- `query = contacts on that config, flatMap { recentHintsFor(it.userId, now) }`,
  deduped with the existing `dedupeHints` helper.
- Map each returned hint back to its contact with the existing
  `contactMatchingHint` logic pattern, then
  `MeshConnectivityStatus.mergeLastSeen(contactHex, localNow - (nowMs - lastSeenMs))`.
- Presence failure must NOT fail the sync pass — wrap in its own try/catch and
  log; message delivery always outranks presence.

Cost check at N=100 contacts: query ≈ 200 hints ≈ 2.5 KB request, response
≤ 200 rows ≈ 6 KB, once per 60 s, one HTTP round trip. Negligible.

### 6.5 Settings toggle

`ProfileScreen`: switch **"Share when I'm online"**, default ON, persisted
next to the other profile prefs. OFF = skip `announce` (others see you drift
offline); `query` still runs so you keep seeing them.

---

## 7. Phasing (each phase ships independently)

**Phase 0 — prerequisite:** 1:1 BLE muling (`BLE_1TO1_MULING.md`). Not part
of this feature, but `MESH_CARRY` is dishonest without it (§2.3).

**Phase 1 — local signals only, no wire/protocol changes.**
`ContactReachability` + `MeshConnectivityStatus` + `helloedUserIds()` + all UI
(§3), with `contactLastSeen` fed only by HELLOs and received-message/receipt
events, and relay health from the existing sync pass. Delivers: pill upgrade,
green NEARBY dots, relay-✓, RECENT from BLE evidence, and MESH_CARRY (if
Phase 0 has landed; otherwise ship with `MESH_CARRY_ENABLED = false`).
`ONLINE_RELAY` can't occur yet (no presence data) — the enum and UI handle it
from day one.

**Phase 2 — relay presence.** §6 end to end: relayd endpoint + migration,
`RelayClient.syncPresence`, sync-pass integration, settings toggle. Lights up
blue `ONLINE_RELAY` and relay-fed `RECENT`.

**Phase 3 — candidates, not committed:** BLE RSSI → bar-strength within
NEARBY; HELLO capability flags ("I have internet") to split MESH_CARRY into
"mule with uplink" (near-relay-grade) vs. "mule, store-and-forward only";
presence-by-proxy (a device announces hints of BLE neighbors it can reach, so
a phone with no internet sitting next to a muling family member shows as
reachable to *remote* senders too); iOS parity.

Phase 1 and 2 are disjoint enough for two agents **except** both touch
`MeshService.performRelaySyncPass` and `MainActivity` — sequence them or give
Phase 2 to the same agent.

---

## 8. Testing

Unit (pure, no device):
- `ContactReachabilityTest`: every level boundary with an injected clock —
  exactly at/past `PRESENCE_ONLINE_WINDOW_MS` and `RECENT_WINDOW_MS`;
  `directLink` trumping everything; `selfRelayHealthy=false` suppressing
  `ONLINE_RELAY` but not `RECENT`; null lastSeen + peers nearby → `MESH_CARRY`;
  null lastSeen + no peers → `OFFLINE`; `MESH_CARRY_ENABLED=false` collapsing
  `MESH_CARRY` to `OFFLINE`.
- `MeshRouterStateTest`: `helloedUserIds()` dual-role dedupe (§5.1).
- Pill-text builder: one test per row of the §3.2 table (extract the builder
  into a pure function, e.g. `MeshStatusTextLogic`, so it's testable like
  `ChatListLogic`).
- relayd (`lib.rs` tests, existing `test_app()` harness): announce-then-query
  round trip; unknown hints omitted; cross-family-token isolation; 48 h prune;
  server-timestamp monotonicity.

Manual/fleet (two test phones):
- Phones adjacent, mesh on → both show `1 nearby`, green dot on each other,
  chat header "Nearby via Bluetooth".
- Kill BLE on one (Bluetooth off) with both on internet → dot green→blue
  within ~2 min (poll cadence), header "Online via relay".
- Airplane-mode one phone → other shows amber "Active {n}m ago", then (with a
  third phone still HELLO'd nearby) the hollow `MESH_CARRY` ring after 15 min,
  or no-dot with nobody around — without reopening the app (30 s ticker
  working).
- Pixel 10 Pro caveat: it runs an always-on full-tunnel VPN — relay health
  must be evaluated through the existing `relayBindTarget()` rules, never a
  separate connectivity probe, or the pill will lie on that device.

Acceptance: no new `Handler` timers or alarms in `MeshService`; presence adds
exactly one HTTP request per sync pass; no UI polling while the activity is
backgrounded; all copy matches §3 verbatim.

---

## 9. Deferred / open questions

- Presence fan-out to multiple relay configs: v1 announces/queries on every
  distinct config (same as mailbox polling). Fine at one-family scale.
- Should `RECENT_WINDOW_MS` be user-visible ("last seen") vs. fuzzy ("recently
  active")? v1 ships fuzzy copy in the list, exact minutes in the chat header
  and contact sheet.
- `ONLINE_RELAY` promises delivery via relay, which also assumes *they* keep
  polling. A device that announces then loses internet shows blue for up to
  150 s. Accepted: windows are tuned to the 60 s poll so the lie is short.
- Group aggregate ignores non-member mules (§2.4) — revisit with Phase 3
  capability flags.
