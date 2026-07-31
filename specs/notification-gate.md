# Notification reliability gate — what it means and what enforces it

ROADMAP.md, "Near-term focus":

> **Notification reliability as a release gate:** background delivery must
> produce a timely local notification on real devices (screen off, battery
> saver, hours idle) before the app is offered to anyone beyond the
> development family. The incumbent apps' single most common failure is
> "the message arrived and nobody knew" — this project refuses to ship that.

Audited 2026-07-30, at the point where the Android tester funnel is being
pushed to its first 12 opt-ins — i.e. exactly when "offered to anyone beyond
the development family" starts being true.

The gate has two halves. This document separates them, because only one of
them can ever be enforced by CI, and conflating them is how a gate quietly
stops meaning anything.

---

## Half 1 — the delivery path asks for a notification (CI-enforceable)

### Verdict: **HOLDS**, and is now enforced.

**Finding: one choke point, reached by every transport.** All five arrival
transports converge on a single decision site. `MessageArrivalMetadata.kt`
enumerates them — `BLE_DIRECT`, `BLE_MULED`, `RELAY`, `LAN_DIRECT`,
`LAN_MULED` — and every one arrives through
`InboundEnvelopeProcessor.processInboundEnvelope`:

| Path in | Entry point | Reaches the notify decision |
|---|---|---|
| BLE direct / muled | `MeshService` frame dispatch → `processInboundEnvelope` | yes |
| LAN direct / muled | same frame dispatch (transport differs, code does not) | yes |
| Relay fetch | `RelaySyncEngine` → `handleRelayEnvelope` → `processInboundEnvelope` | yes |

There is no second insertion path: the only three `insertIncomingMessage`
call sites in the app are all inside `InboundEnvelopeProcessor`, and the two
that store user-visible chat messages both make the notify decision inline
immediately afterward (1:1 in `handleIncomingChatMessage`, group in
`handleIncomingGroupChatMessage`). **A message cannot be stored by one
transport and silently skip the notification decision taken for another.**
That is the strongest structural claim the gate needs, and it is true.

**Finding: the decision itself had zero test coverage — because it was
structurally untestable.** `MessageNotifier` reaches straight for
`Context.getSystemService` and `Base64`, both of which throw on the bare JVM
(there is no Robolectric here). So the notify branch was the one branch of
the inbound path no unit test could execute, and the suite had grown three
separate workarounds *around* it:

- `GroupFanoutRelayDeliveryTest` marks the chat on-screen before delivering,
  with the comment *"skips MessageNotifier, whose Base64 call NPEs on the
  JVM… the production notification path is not what this test pins."*
- `BlockedSenderTest` and `GroupMembershipEnforcementTest` wrap delivery in
  `runCatching { }` — which swallows the notifier's throw, and would equally
  swallow a genuine delivery failure.

Net effect: **deleting the notification call outright would not have failed a
single test.** The release gate was enforced by nothing.

### What this branch changed

`IncomingMessageAnnouncer` — a four-method sink interface, injected into
`InboundEnvelopeProcessor` with the production implementation
(`NotificationAnnouncer` → `MessageNotifier`) as the constructor default, so
no production call site changes. Same pattern the class already uses for
`LanHooks`.

**No behavior change.** Suppression policy stays exactly where it was: the
on-screen check at the call site (`ChatVisibility`), per-chat mute and the
`POST_NOTIFICATIONS` check inside `MessageNotifier`.

`NotificationReleaseGateTest` then pins the invariant on the real processor
and the real core:

| Scenario | Asserted |
|---|---|
| Renderable 1:1 message, chat off screen | announces **exactly once** |
| Same envelope delivered twice (digest re-offer) | announces **once, not twice** |
| Message for the chat currently on screen | announces **zero** times |
| Message for chat B while chat A is on screen | still announces (suppression is per-chat) |
| Group message, group chat off screen | announces **exactly once**, naming the sender |

Verified with a mutation check: commenting out the 1:1 notify call makes
**3 of the 5 tests fail**. Before this branch it made nothing fail.

Full Android unit suite after the change: **64 suites, 382 tests, 0 failures,
0 skipped.** No Rust was touched.

---

## Half 2 — Android then actually shows it (NOT CI-enforceable)

### Verdict: **UNVERIFIED. Needs two phones. Nothing here can close it.**

Everything above proves the app *asks* for a notification. It cannot prove
Android *delivers* one, and the gate's own wording is about the part that
can't be automated here: *screen off, battery saver, hours idle*. The
failure modes that live in this half are all environmental, and every one of
them is invisible to a JVM test:

- Doze / App Standby buckets deferring the foreground service's work after
  hours of idle.
- OEM battery managers (notably aggressive on some Android skins) killing or
  freezing the service.
- `POST_NOTIFICATIONS` denied or later revoked — `MessageNotifier` logs at
  INFO and returns, which is correct behavior and an invisible gate failure.
- Notification channel importance downgraded by the user, so it arrives
  without heads-up or sound: technically delivered, functionally "nobody
  knew."
- The message channel is `IMPORTANCE_HIGH` and the foreground-service channel
  is `IMPORTANCE_LOW`; they share notification id `1` and are kept distinct
  only by the tag on the message notification. Worth an eyeball on a real
  device that one never replaces the other.

**The protocol to close this half is a scripted two-phone run, not a code
review.** It is written up for whoever has the phones; see the scout
`FOR-DAVID.md`. Until that run is recorded, the honest status of the ROADMAP
gate is *"code half proven, device half unproven"* — which is a much better
place to be than before this audit, and still short of shippable.

---

## Open question this audit surfaced (product, not a bug)

The 1:1 and group paths **disagree about unknown senders.**

- 1:1 (`handleIncomingChatMessage`): a message from a userId that is not a
  contact is stored, then the path returns early — *no receipt and no
  notification*, deliberately, with a comment explaining there is no display
  name and no key to trust it came from who it claims.
- Group (`handleIncomingGroupChatMessage`): an unknown sender still notifies,
  falling back to the first 8 hex characters of their userId as a name.

Both are defensible. But the 1:1 case is reachable — the code comment itself
notes "friending can happen independently of messaging order" — and it is a
real instance of *stored, and nobody knew*. It is likelier during a tester
funnel, where people friend each other in mixed order, than in steady state.

Not changed here: the two behaviors are intentional as written and picking
between them is a product call, not a 1 a.m. refactor. Flagged for a decision.
