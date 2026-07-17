# CruiseMesh DTN Effectiveness Audit

Audited at master `68abbe1` (post LAN-transport merges), 2026-07-16.
Scope: how well the system performs as an opportunistic, delay-tolerant
messaging network across its three transports — BLE mesh, internet relay,
and same-LAN TCP — in both the strong-connection case and the delayed case.

## Verdict

The architecture is a textbook DTN design and the implementation is
unusually faithful to it. Every layer assumes duplicates, reordering, and
hours-scale latency; dedup by `msg_id` exists at every boundary (seen-ID
set, SQLite `INSERT OR IGNORE`, relay `(family_token, msg_id)` unique key);
and durability is anchored at the author: a message is committed to the
local store *atomically with its sealed retry copy* before any transport is
attempted, and stays queued until a cumulative DELIVERED receipt covers it
or its 7-day expiry passes. The weak spots are latency in the
strong-connection relay case (poll-only, no push), a few places where
redundancy is shed earlier than it needs to be, and validation gaps rather
than design gaps.

## What works well (verified in code)

1. **Durable-first send path** (`chat/MeshSender.kt`): store plaintext row +
   sealed outbound envelope in one transaction, *then* attempt delivery.
   Transport failure can't lose an authored message. Every new send also
   replays all still-unacked envelopes for that chat in lamport order —an
   incidental in-session retry mechanism.

2. **Digest sync heals everything on reconnect** (`handleDigest`):
   per-sender highest-contiguous-lamport watermarks + a recent-`msg_id`
   list. Receipts go first (smallest, unblock the most UI), then missing
   messages, then group catch-up, then three spray hooks (carried foreign
   envelopes, own pending outbound to *other* contacts via any mule, own
   pending receipts via any mule). Safe to interrupt mid-transfer; reconnect
   re-runs it.

3. **Cumulative receipts** ("delivered/read through lamport N"): idempotent,
   re-sent on every sync and relay pass, so one lost receipt self-heals and a
   single receipt confirms a backlog. Receipts also travel by mule
   (`sprayOwnPendingReceiptsTo`), so ticks can update over a pure-offline
   A→mule→C→mule→A round trip.

4. **Carry queue matches the spec** (`store.rs`): family envelopes (hint
   matches a contact/group) are evict-proof until expiry; foreign envelopes
   share a 5 MB budget with oldest-first eviction, all in one transaction.
   Expiry (7 days) and hop TTL (7, decremented only on live forwarding, not
   on physical carry — correct DTN semantics) are enforced on receive,
   forward, and prune. (The `protocol.rs` module comment saying MeshService
   "doesn't act on hop_ttl/expiry yet" is stale — it does.)

5. **Relay is a safe durable mailbox**: 30-day server-side retention ceiling
   clamping the client expiry, prune-on-fetch, per-family token auth,
   delete-only-on-ack. The client acks only `CONSUMED`/`EXPIRED`
   dispositions — a proxied (`CARRIED`) or already-`SEEN` envelope is never
   acked, so the relay copy stays the durable fallback. Relay proxy-polling
   (fetching contacts' hints alongside your own) correctly bridges the
   "recipient has no internet, sender's BLE cluster never meets theirs"
   case: one paid-Wi-Fi phone uplinks and downlinks for the whole family.

6. **Multi-transport arbitration** (`MeshRouter.transportSendPlan`): small
   frames race LAN + one BLE route (one bad path doesn't add a retry delay);
   frames >8 KB go LAN-only so photos don't duplicate over BLE. LAN teardown
   preserves BLE routes and vice versa. LAN peers are Noise-XX-authenticated
   against accepted contacts before any mesh frames flow, with probe-based
   (3-strike) stale-socket detection and reconnect backoff.

7. **Restart hardening**: the in-memory 50k-entry seen-ID set is reseeded
   from own outbound history + carried msg_ids on service start, so a mule
   handing your own envelope back post-restart isn't misclassified as
   foreign traffic.

## Findings (ranked)

### F1 — Relay latency when connections are strong: poll-only, 60 s
Both clients poll the relay every 60 s (`RELAY_POLL_INTERVAL_MS`); neither
uses the WebSocket push hub that **relayd already implements**
(`WS_BROADCAST_CAPACITY`, broadcast on POST). Two phones that are both
internet-connected (in port, at home pre/post-cruise) see up to ~60 s
one-way latency where a WS subscription would give near-instant delivery.
This is the single biggest gap against "works great when connections are
strong." Uploads are event-driven (send triggers `requestSync`), so the
cost is on the receive side only. *Fix: subscribe to the existing relayd WS
broadcast when validated internet is present; keep the 60 s poll as
fallback.*

### F2 — Mule deletes its carried copy on dispatch, not on confirmation
`drainCarriedEnvelopesTo` removes a 1:1 carried envelope as soon as
`MeshRouter.sendToAddress` returns true — but dispatch is fire-and-forget
(`dispatch()` invokes the transport's `send(address, frame)` which returns
`Unit`), and BLE teardown drops the per-address write queue
(`BleCentral.tearDownLink` → `writeQueues.remove`). If the link dies with
fragments queued, the mule's copy is gone and the recipient got nothing.
Not fatal — the author still holds the envelope and re-sprays, and a relay
copy may exist — but it discards exactly the redundancy a DTN mule exists
to provide, and the loss window (BLE disconnect mid-drain) is the *common*
case on a ship, not the rare one. *Fix options: remove the carried copy
only after the write queue for that address fully drains; or treat the
peer's next digest (msg_id now in their recent list) as the removal
trigger.*

### F3 — Envelopes delivered over BLE/LAN are never acked off the relay
`processInboundEnvelope` records `msg_id` in the seen-set before opening.
When the same envelope is later fetched from the relay (sender uploaded it
too), disposition is `SEEN` → never acked → re-fetched on **every 60 s poll
pass** (cursor restarts at 0 each pass) until it expires server-side or the
app restarts (restart reseeding omits consumed incoming msg_ids, so the
next fetch opens → `CONSUMED` → acked). On metered ship Wi-Fi this is real
wasted bandwidth that grows with chat activity. *Fix: ack `SEEN` when
`store.messageByMsgId` shows we already consumed it — distinguishing "seen
because I stored it" from "seen because I'm merely carrying it."* The
in-code TODO about persistent proxy cursors would also mitigate.

### F4 — Seen-set poisoning on carry failure (minor)
`checkAndRecord` fires before carry/store; if `enqueueCarriedEnvelope`
throws (disk full, DB error), the msg_id is still marked seen, so every
future copy on any link is dropped as `SEEN` for the rest of the process
lifetime. Low probability, but the failure and the dedup key are coupled
where they don't need to be.

### F5 — In-session frame loss heals only at the next sync event
Fragment reassembly (`FrameReassembler`) drops a frame on any gap and BLE
notifications are unacked; there's no periodic re-digest on a live link.
A frame lost mid-session is recovered only on (a) the next reconnect's
digest exchange, (b) the sender's next authored send (which replays all
pending), or (c) a relay pass. All three exist, so nothing is lost — but a
quiet, stable link can sit minutes-to-hours with an undelivered message
both sides think is in flight. *Cheap fix: re-run the digest exchange on a
long-lived link every few minutes, or when `TRANSPORT_PROBE` already
round-trips anyway (LAN has this; BLE doesn't).*

### F6 — 7-day expiry is a hard delivery deadline everywhere
`DEFAULT_EXPIRY_MS` bounds the author's own outbound queue
(`pruneExpiredOutboundEnvelopes`), not just mule/relay copies. A message
authored on day 1 of a 10-day cruise to someone unreachable until day 9
silently dies — no UI surfaces "expired undelivered" vs "still trying"
(ticks just stay at ✓). Reasonable default; the gap is *visibility*, and
that the author's own copy doesn't need to expire at all (only carried and
relay copies are a shared-resource cost).

### F7 — Group sync is lamport-0 resend + local-only receipts
No per-group digests: every reconnect re-offers a group's entire outbound
history (`resendGroupOutboundToPeer`), and receiver-side dedup absorbs it.
Fine at family scale, quadratic-ish churn beyond it. Group delivery/read
ticks are local-only on both platforms, so group "did it get through?" —
the app's core promise — is weaker than 1:1.

### F8 — Wall-clock dependence
Expiry checks and daily-rotating `recipient_hint`s both trust the local
clock. The 7-day hint window (`CARRY_HINT_DAY_WINDOW`) absorbs modest skew
and cruise-timezone drift, but a device with a badly wrong clock will
misclassify family traffic as foreign (evictable) and mis-drop/mis-keep on
expiry. No skew detection exists. Low priority given phones NTP-sync.

### F9 — Validation gates for LAN transport are still open
`specs/same-lan-transport.md` lists seven pre-default-on gates (client
isolation fallback, captive portals, roaming reconnect, screen-off
behavior, cross-platform delivery, battery). None are marked done. The
design degrades cleanly (discovery just fails → BLE/relay continue), but
"effectiveness on an actual ship LAN" is currently unproven, and ship
networks are exactly where client isolation is common.

### F10 — Platform ceilings (documented, unchanged)
iOS backgrounding remains the existential risk the design itself flags:
LAN transport runs only while iOS grants execution; BLE background is
best-effort; relayd's WS (once used) won't run backgrounded either. Android
is stronger (foreground service + boot receiver) but the mesh stops when
the user kills the service. These bound worst-case delay tolerance more
than any protocol choice does.

## Scorecard

| Scenario | Effectiveness |
|---|---|
| Direct BLE, both foregrounded | Strong — instant, digest-healed, receipt-confirmed |
| Same LAN (permissive network) | Strong — races LAN+BLE, Noise-authenticated, photo-capable; unvalidated on real ship Wi-Fi (F9) |
| Both online via relay | Good but laggy — up to ~60 s one-way from poll-only (F1) |
| Mule / store-carry-forward | Good — carry queue + 3 spray hooks + proxy-polling; weakened by delete-on-dispatch (F2) |
| Long delays (days) | Good to 7 days, silent death after (F6); iOS background is the real ceiling (F10) |
| Duplicate/loop suppression | Excellent — layered dedup, TTL, budget-bounded carry |
| Efficiency on metered links | Moderate — SEEN-never-acked refetch churn (F3), group lamport-0 resend (F7) |

## Suggested priority

1. F1 (WS push — biggest UX win, server side already done)
2. F2 (mule delete-on-dispatch — core DTN guarantee)
3. F3 (relay ack for consumed-elsewhere — metered bandwidth)
4. F5 (periodic re-digest on live links)
5. F6 (expiry visibility in UI)
6. F9 (run the LAN validation gates on a real permissive/isolated network)
